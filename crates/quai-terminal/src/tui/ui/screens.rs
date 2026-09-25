//! The core screens: activity, accounts, Qi coins, locks, contacts and channels, network, settings.

use super::*;
use wallet_core::journal::OpKind;

// ---------------------------------------------------------------- screens

pub(crate) fn incoming_text(a: &Activity) -> String {
    let v: U256 = a.amount.parse().unwrap_or_default();
    match a.asset.as_str() {
        "QI" => format!("{} Qi", qi(v)),
        "QUAI" => format!("{} QUAI", q(v)),
        other => {
            let dec = a.detail.decimals().as_u64().unwrap_or(18) as u8;
            format!("{} {other}", super::super::num::short(v, dec, 4))
        }
    }
}

/// Contact whose payment code or address is `key` (addresses compare case-insensitively).
pub(crate) fn contact_matching<'a>(app: &'a App, key: &str) -> Option<&'a str> {
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    app.dash
        .contacts
        .iter()
        .find(|c| c.payment_code.as_deref() == Some(key) || c.address.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(key)))
        .map(|c| c.name.as_str())
}

/// The contact an operation was with: its payment-code peer, else its counterparty.
pub(crate) fn op_contact<'a>(app: &'a App, op: &Operation) -> Option<&'a str> {
    op.detail.peer().as_str().and_then(|p| contact_matching(app, p)).or_else(|| contact_matching(app, &op.counterparty))
}

/// The contact who sent an incoming payment, when the channel is known.
pub(crate) fn activity_contact<'a>(app: &'a App, a: &Activity) -> Option<&'a str> {
    if let Some(peer) = a.detail.peer().as_str() {
        return contact_matching(app, peer);
    }
    // Rows recorded before the full code was kept only carry `payment from <short code>`.
    let short = a.detail.origin().as_str()?.strip_prefix("payment from ")?;
    app.dash.contacts.iter().find(|c| c.payment_code.as_deref().is_some_and(|code| short_code(code) == short)).map(|c| c.name.as_str())
}

/// Icon for the token, NFT collection or native coin an activity row is about.
pub(crate) fn row_badge(app: &App, t: &Theme, symbol: &str, detail: &wallet_core::journal::Detail) -> Option<Span<'static>> {
    let Some(contract) = [detail.contract(), detail.token(), detail.to_token()]
        .into_iter()
        .find_map(|v| v.as_str().filter(|c| c.starts_with("0x")))
        .map(str::to_lowercase)
    else {
        return match symbol.to_ascii_lowercase().as_str() {
            native @ ("quai" | "qi") => Some(super::super::images::native_span(app, t, native)),
            _ => None,
        };
    };
    // NFTs are badged by collection name; fungible tokens by symbol (their name may differ).
    let nft = detail.token_id().as_str().is_some_and(|id| !id.is_empty());
    let name = detail.name().as_str().filter(|n| nft && !n.is_empty()).unwrap_or(symbol);
    let icon = app.asset_icon_url(&contract);
    Some(super::super::images::badge_span(app, t, icon.as_deref(), name, &contract))
}

pub(crate) fn with_badge(badge: Option<Span<'static>>, text: String) -> Line<'static> {
    match badge {
        Some(b) => Line::from(vec![b, Span::raw(" "), Span::raw(text)]),
        None => Line::from(text),
    }
}

/// Confirmations shown as a tally until a mined operation reaches this depth.
pub(crate) const CONFIRM_TARGET: u64 = 5;

/// (confirmations, target) for a mined operation still short of the target.
pub(crate) fn confirmations(op: &Operation, head: u64) -> Option<(u64, u64)> {
    if !matches!(op.status, OpStatus::Confirmed | OpStatus::Settled | OpStatus::Settling | OpStatus::Locked) {
        return None;
    }
    let included = op.detail.included_block().as_u64()?;
    let n = head.checked_sub(included)? + 1;
    (n < CONFIRM_TARGET + 1).then_some((n.min(CONFIRM_TARGET), CONFIRM_TARGET))
}

/// `━━━╍╍ 3/5`: filled segments per confirmation.
/// A confirming transaction's tally with the segment the last block added lit for a moment
/// (Full and Vivid): the news where it is, then still. `None` when nothing is lit.
pub(crate) fn tally_lit(app: &App, t: &Theme, op: &Operation) -> Option<Line<'static>> {
    // Lit as long as the header's block pulse, which ends it with the same redraw.
    let fresh = app.fx.beat.is_some_and(|b| b.elapsed() < BEAT_PULSE);
    if !fresh || !app.motion().effects() {
        return None;
    }
    let head = app.dash.health.as_ref().map(|h| h.height).unwrap_or(0);
    let (n, target) = confirmations(op, head).filter(|(n, target)| *n > 0 && n < target)?;
    let glint = super::super::edge::ramp(t).map(|(_, _, g)| Color::Rgb(g.0, g.1, g.2)).unwrap_or(t.strong);
    let pending = Style::default().fg(t.pending);
    Some(Line::from(vec![
        Span::styled(format!("{} {}", status_glyph(t, op.status), "━".repeat(n as usize - 1)), pending),
        Span::styled("━", Style::default().fg(glint).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{} {n}/{target}", "╍".repeat(target.saturating_sub(n) as usize)), pending),
    ]))
}

pub(crate) fn tally(n: u64, target: u64) -> String {
    format!("{}{} {n}/{target}", "━".repeat(n as usize), "╍".repeat(target.saturating_sub(n) as usize))
}

/// What is happening with an unfinished operation, and whether it waits on the user.
pub(crate) fn op_next_step(op: &Operation, head: u64) -> String {
    let unlock = op.detail.unlock_height().as_u64().filter(|u| *u > head);
    match op.status {
        OpStatus::Prepared | OpStatus::Signed => "not sent yet · it is submitted when you approve the review".into(),
        // Both ledgers can be replaced now, so neither arm singles one out. A Qi replacement pays
        // its higher fee out of the transaction's own change rather than from the account.
        OpStatus::Submitted => "waiting to be mined · u speeds it up with a higher fee".into(),
        OpStatus::Unknown => "submission not confirmed yet · the wallet re-checks the chain on every refresh · u speeds it up".into(),
        OpStatus::Settling => "mined · waiting for the destination to settle · nothing to do".into(),
        OpStatus::Locked => match unlock {
            Some(u) => format!(
                "mined · locked by the protocol until block {} (~{}) · becomes spendable automatically · nothing to do",
                amount::group_thousands(&u.to_string()),
                wallet_core::track::human_duration((u - head) * 5)
            ),
            None => "mined · locked by the protocol · becomes spendable automatically · nothing to do".into(),
        },
        _ => String::new(),
    }
}

pub(crate) fn op_row_parts(app: &App, t: &Theme, op: &Operation) -> (Span<'static>, String, Span<'static>, String) {
    let head = app.dash.health.as_ref().map(|h| h.height).unwrap_or(0);
    let status = match confirmations(op, head).filter(|(n, target)| n < target) {
        Some((n, target)) => Span::styled(format!("{} {}", status_glyph(t, op.status), tally(n, target)), Style::default().fg(t.pending)),
        None => Span::styled(format!("{} {}", status_glyph(t, op.status), op.status.as_str()), status_style(t, op.status)),
    };
    let icon_color = if op.asset.eq_ignore_ascii_case("QI") { t.qi } else { t.quai };
    let what = match op_contact(app, op) {
        Some(name) if op.kind != OpKind::Notify => format!("{} → {name}", describe(op)),
        Some(name) => format!("mailbox notify to {name}"),
        None => describe(op),
    };
    (Span::styled(kind_icon(t, &op.kind), Style::default().fg(icon_color)), what, status, op.tx_hash.clone().unwrap_or_default())
}

/// Value and gas for a row, as detail lines: what the transaction carried in its native coin
/// (QUAI, or Qi on the UTXO ledger) and what it cost to send. Labels are the second element's
/// keys, so both detail views say the same thing.
pub(crate) fn cost_lines(app: &App, t: &Theme, op: Option<&Operation>, seen: Option<&Activity>) -> Vec<(&'static str, Span<'static>)> {
    use wallet_core::track::{native_text, op_cost};
    let dim = t.dim_style();
    if let Some(op) = op {
        let cost = op_cost(op);
        let value = match cost.value {
            Some(v) => Span::styled(cost.text(v), t.text_style()),
            None => Span::styled("—", dim),
        };
        let fee = match (cost.fee, cost.fee_final) {
            (Some(f), true) => Span::styled(cost.text(f), t.text_style()),
            (Some(f), false) => Span::styled(format!("up to {} · not mined yet", cost.text(f)), Style::default().fg(t.pending)),
            (None, _) => Span::styled("—", dim),
        };
        return vec![("value", value), (if cost.qi { "fee" } else { "gas" }, fee)];
    }
    let Some(a) = seen else { return Vec::new() };
    let incoming = a.direction == "in";
    let qi = a.asset == "QI";
    let known = a.tx_hash.as_ref().and_then(|h| app.eco.feeds.tx_costs.get(h)).and_then(|r| r.latest());
    let reading = || Span::styled(format!("{} reading…", spinner()), dim);
    // A native row states its own value; a token row's native value comes from the transaction.
    let value = match (a.asset.as_str(), known) {
        ("QUAI" | "QI", _) => Span::styled(native_text(a.amount.parse().unwrap_or_default(), qi), t.text_style()),
        (_, Some(Ok(c))) => c.value.map_or_else(|| Span::styled("—", dim), |v| Span::styled(c.text(v), t.text_style())),
        (_, Some(Err(_))) => Span::styled("—", dim),
        (_, None) if a.tx_hash.is_some() => reading(),
        _ => Span::styled("—", dim),
    };
    let sender = if incoming { " · paid by the sender" } else { "" };
    let fee = match known {
        _ if qi => Span::styled(if incoming { "paid by the sender" } else { "—" }, dim),
        Some(Ok(c)) => c.fee.map_or_else(|| Span::styled("—", dim), |f| Span::styled(format!("{}{sender}", c.text(f)), t.text_style())),
        Some(Err(e)) => Span::styled(truncate(&app::friendly_error(e), 40), dim),
        None if a.tx_hash.is_some() => reading(),
        None => Span::styled("—", dim),
    };
    vec![("value", value), (if qi { "fee" } else { "gas" }, fee)]
}

/// The fee for a table row, compact: what was paid, `≤` the most it can cost while unmined.
/// Incoming rows are blank — their gas was the sender's.
pub(crate) fn fee_cell(app: &App, t: &Theme, op: Option<&Operation>, seen: Option<&Activity>) -> Span<'static> {
    if let Some(op) = op {
        let cost = wallet_core::track::op_cost(op);
        return match (cost.fee, cost.fee_final) {
            (Some(f), true) => Span::styled(cost.text(f), t.dim_style()),
            (Some(f), false) => Span::styled(format!("≤{}", cost.text(f)), Style::default().fg(t.pending)),
            (None, _) => Span::raw(""),
        };
    }
    match seen {
        Some(a) if a.direction == "out" => match a.tx_hash.as_ref().and_then(|h| app.eco.feeds.tx_costs.get(h)).and_then(|r| r.latest()) {
            Some(Ok(c)) => c.fee.map_or_else(|| Span::raw(""), |f| Span::styled(c.text(f), t.dim_style())),
            _ => Span::raw(""),
        },
        _ => Span::raw(""),
    }
}

/// Activity rows. `compact` is the width the description may take: a narrow panel (Home) gets a
/// four-cell age, the description cut with an ellipsis where it has to be, and the status as its
/// mark alone — the full words, gas and hash are on the Activity screen.
pub(crate) fn activity_table_rows<'a>(app: &App, t: &Theme, limit: usize, labels: bool, compact: Option<usize>) -> Vec<Row<'a>> {
    let rows = app.activity_rows();
    let offset = if labels { app.list_window(app.main_list(), app.nav.selected, rows.len(), limit) } else { 0 };
    rows.iter()
        .enumerate()
        .skip(offset)
        .take(limit.max(1))
        .map(|(i, (time, is_op, idx))| {
            let badge = if *is_op {
                let op = &app.dash.ops[*idx];
                row_badge(app, t, &op.asset, &op.detail)
            } else {
                let a = &app.dash.activity[*idx];
                row_badge(app, t, &a.asset, &a.detail)
            };
            let (icon, what, status, hash) = if *is_op {
                op_row_parts(app, t, &app.dash.ops[*idx])
            } else {
                let a = &app.dash.activity[*idx];
                let color = if a.asset == "QI" { t.qi } else { t.quai };
                let planted = app.dust_from_stranger(a);
                let outgoing = a.direction == "out";
                let (joiner, verb) = if outgoing {
                    ("to", format!("{} sent", t.icon(Icon::Ok)))
                } else {
                    ("from", format!("{} received", t.icon(Icon::Ok)))
                };
                let what = match activity_contact(app, a) {
                    Some(name) => format!("{} {joiner} {name}", super::super::views::activity_text(a)),
                    None => {
                        format!(
                            "{} {} {}",
                            super::super::views::activity_text(a),
                            if outgoing { "from" } else { "→" },
                            short_address(&a.address)
                        )
                    }
                };
                // Dust from an address never sent to is how a lookalike gets into the history: say so
                // where it sits, not only when someone tries to send to it.
                let status = if planted {
                    Span::styled(format!("{} dust · likely a lookalike", t.icon(Icon::Warning)), Style::default().fg(t.attention))
                } else {
                    Span::styled(verb, Style::default().fg(t.ok))
                };
                (
                    Span::styled(t.icon(if outgoing { Icon::Send } else { Icon::Receive }), Style::default().fg(color)),
                    what,
                    status,
                    a.tx_hash.clone().unwrap_or_default(),
                )
            };
            let mut cells = Vec::new();
            if labels {
                cells.push(Cell::from(Span::styled(app::jump_label(i - offset).to_string(), jump_style(app, t))));
            }
            if let Some(width) = compact {
                let mark = status.content.chars().next().map(String::from).unwrap_or_default();
                let cells = vec![
                    Cell::from(Line::from(Span::styled(ago_short(*time), t.dim_style())).alignment(Alignment::Right)),
                    Cell::from(icon),
                    Cell::from(with_badge(badge, truncate(&what, width.saturating_sub(3)))),
                    Cell::from(Span::styled(mark, status.style)),
                ];
                return Row::new(cells);
            }
            cells.push(Cell::from(Span::styled(ago(*time), t.dim_style())));
            cells.push(Cell::from(icon));
            cells.push(Cell::from(with_badge(badge, what)));
            let lit = if *is_op { tally_lit(app, t, &app.dash.ops[*idx]) } else { None };
            cells.push(Cell::from(lit.unwrap_or_else(|| Line::from(status))));
            let fee =
                if *is_op { fee_cell(app, t, app.dash.ops.get(*idx), None) } else { fee_cell(app, t, None, app.dash.activity.get(*idx)) };
            cells.push(Cell::from(Line::from(fee).alignment(Alignment::Right)));
            cells.push(Cell::from(Span::styled(if hash.is_empty() { String::new() } else { short_address(&hash) }, t.dim_style())));
            let row = Row::new(cells);
            // A row that just reached its confirmation target lights once, fading out (background
            // only; the text keeps its colors).
            // An arrival lights its new row the same way.
            let flash_key = if *is_op { &app.dash.ops[*idx].id } else { &app.dash.activity[*idx].key };
            let flash = app.fx.row_flash.get(flash_key).map(|s| s.elapsed().as_millis());
            if labels && i == app.nav.selected {
                row.style(t.selected())
            } else if let Some(bg) = flash.and_then(|ms| super::super::edge::flash_bg(t, t.ok, ms)) {
                row.style(Style::default().bg(bg))
            } else {
                row
            }
        })
        .collect()
}

pub(crate) fn draw_accounts(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let (area, inspector) = with_inspector(app, area);
    let n_accounts = app.dash.accounts.len();
    // The time locks sit under the accounts: they belong to the wallet, not to one account, and
    // there are rarely more than a few. Each panel takes the rows it has and no more.
    let locks_h = (app.dash.locks.len().max(1) as u16 + 2).min(area.height / 3).max(3);
    let accounts_h = if n_accounts == 0 { 7 } else { n_accounts as u16 + 3 }.min(area.height.saturating_sub(locks_h));
    let [acct_area, locks_area, rest] =
        Layout::vertical([Constraint::Length(accounts_h), Constraint::Length(locks_h), Constraint::Min(0)]).areas(area);
    draw_locks(f, app, t, locks_area);
    // Beside the list when the terminal is wide; under it when there are rows to spare.
    match inspector {
        Some(column) => draw_account_inspector(f, app, t, column),
        None if rest.height >= 9 => draw_account_inspector(f, app, t, rest),
        None => {}
    }
    let narrow = acct_area.width < 110;
    let block = panel(t, "quai accounts", true);
    let inner = block.inner(acct_area);
    f.render_widget(block, acct_area);
    if n_accounts == 0 {
        empty(f, inner, t, Icon::Wallet, "No accounts loaded yet.", &[("a", "add account")]);
    } else {
        // It scrolls: a wallet can have more accounts than the panel has rows.
        let body = Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner };
        let offset = app.list_window(app.main_list(), app.nav.selected, n_accounts, body.height as usize);
        app.input
            .hits
            .borrow_mut()
            .rows(app.main_list(), body, offset, n_accounts, |i| app.dash.accounts.get(i).map(|a| a.address.clone()));
        let rows: Vec<Row> = app
            .dash
            .accounts
            .iter()
            .enumerate()
            .skip(offset)
            .take(body.height as usize)
            .map(|(i, a)| {
                let row = Row::new(vec![
                    Cell::from(Span::styled(app::jump_label(i - offset).to_string(), jump_style(app, t))),
                    Cell::from(Span::styled("▌", Style::default().fg(t.quai))),
                    Cell::from(a.label.clone()),
                    Cell::from(Span::styled(
                        if narrow { short_address(&a.address) } else { a.address.clone() },
                        Style::default().fg(t.link),
                    )),
                    Cell::from(Line::from(Span::styled(q(a.balance), t.strong_style().fg(t.quai))).alignment(Alignment::Right)),
                    Cell::from(if a.locked.is_zero() { String::new() } else { format!("{} {}", t.icon(Icon::Locked), q(a.locked)) }),
                    Cell::from(Span::styled(a.nonce.to_string(), t.dim_style())),
                ]);
                if i == app.nav.selected { row.style(t.selected()) } else { row }
            })
            .collect();
        let addr_w = if narrow { 13 } else { 44 };
        let table = Table::new(
            rows,
            [
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(14),
                Constraint::Length(addr_w),
                Constraint::Length(18),
                Constraint::Length(16),
                Constraint::Length(6),
            ],
        )
        .column_spacing(2)
        .header(
            Row::new(vec![
                Cell::from(""),
                Cell::from(""),
                Cell::from("label"),
                Cell::from("address"),
                Cell::from(
                    Line::from(vec![super::super::images::native_span(app, t, "quai"), Span::raw(" QUAI")]).alignment(Alignment::Right),
                ),
                Cell::from("locked"),
                Cell::from("nonce"),
            ])
            .style(t.dim_style()),
        );
        f.render_widget(table, inner);
    }
}

pub(crate) fn draw_activity(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let (list, detail) = if area.width >= 120 {
        let [l, d] = Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(area);
        (l, d)
    } else {
        let [l, d] = Layout::vertical([Constraint::Min(6), Constraint::Length(11)]).areas(area);
        (l, d)
    };
    let title = if app.nav.jump_pending.is_some() {
        "activity · press a label".to_string()
    } else {
        format!("activity · {}", app.nav.activity_filter.title().to_lowercase())
    };
    let block = panel(t, &title, true);
    let inner = block.inner(list);
    f.render_widget(block, list);
    let rows = activity_table_rows(app, t, inner.height.saturating_sub(1) as usize, true, None);
    if app.activity_rows().is_empty() {
        empty(
            f,
            inner,
            t,
            Icon::Activity,
            "Nothing here yet — sends, receipts and conversions will appear as they happen.",
            &[("r", "receive")],
        );
    } else {
        let table = Table::new(
            rows,
            [
                Constraint::Length(1),
                Constraint::Length(8),
                Constraint::Length(1),
                Constraint::Min(24),
                Constraint::Length(12),
                Constraint::Length(15),
                Constraint::Length(13),
            ],
        )
        .header(
            Row::new(vec![
                Cell::from(""),
                Cell::from("when"),
                Cell::from(""),
                Cell::from("what"),
                Cell::from("status"),
                Cell::from(Line::from("gas").alignment(Alignment::Right)),
                Cell::from("tx"),
            ])
            .style(t.dim_style()),
        );
        f.render_widget(table, inner);
        let body = Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner };
        let all = app.activity_rows();
        app.input
            .hits
            .borrow_mut()
            .rows(app.main_list(), body, app.view_offset(), all.len(), |i| all.get(i).map(|r| app.activity_row_key(r)));
    }
    let rows = app.activity_rows();
    let mut lines: Vec<Line> = Vec::new();
    let kv = |k: &str, v: String| super::super::widgets::kv(t, k, vec![Span::raw(v)]);
    match rows.get(app.nav.selected) {
        Some((_, true, i)) => {
            let op = &app.dash.ops[*i];
            let contact = op_contact(app, op);
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", kind_icon(t, &op.kind)), t.text_style()),
                Span::styled(op_row_parts(app, t, op).1, t.strong_style()),
            ]));
            lines.push(Line::from(Span::styled(
                format!("{} {}", status_glyph(t, op.status), op.status.as_str()),
                status_style(t, op.status),
            )));
            lines.push(kv("operation", op.id.clone()));
            lines.push(kv("from", op.account.clone()));
            if let Some(name) = contact {
                lines.push(kv("contact", name.to_string()));
            }
            lines.push(kv("to", op.counterparty.clone()));
            for (label, value) in cost_lines(app, t, Some(op), None) {
                lines.push(super::super::widgets::kv(t, label, vec![value]));
            }
            if let Some(h) = &op.tx_hash {
                lines.push(kv("tx", h.clone()));
                if let Some(e) = &app.dash.explorer {
                    lines.push(kv("explorer", format!("{}/tx/{h}", e.trim_end_matches('/'))));
                }
            }
            // `native_value` is stated above as the value, in QUAI rather than wei.
            for (k, v) in op.detail.entries().filter(|(k, _)| *k != "native_value").take(10) {
                lines.push(kv(k, v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())));
            }
            if !op.status.is_terminal() {
                let head = app.dash.health.as_ref().map(|h| h.height).unwrap_or(0);
                lines.push(Line::from(""));
                lines
                    .push(Line::from(vec![Span::styled("next  ", t.strong_style()), Span::styled(op_next_step(op, head), t.text_style())]));
            }
        }
        Some((_, false, i)) => {
            let a = &app.dash.activity[*i];
            lines.push(Line::from(vec![
                Span::styled("↘ ", Style::default().fg(t.ok)),
                Span::styled(format!("{} {}", wallet_core::track::incoming_verb(a), incoming_text(a)), t.strong_style()),
            ]));
            if let Some(name) = activity_contact(app, a) {
                lines.push(kv("from", name.to_string()));
            }
            lines.push(kv("to", a.address.clone()));
            for (label, value) in cost_lines(app, t, None, Some(a)) {
                lines.push(super::super::widgets::kv(t, label, vec![value]));
            }
            if let Some(h) = &a.tx_hash {
                lines.push(kv("tx", h.clone()));
            }
            if let Some(b) = a.block {
                lines.push(kv("block", amount::group_thousands(&b.to_string())));
            }
        }
        None => lines.push(Line::from(Span::styled("select a row to see details", t.dim_style()))),
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(panel(t, "detail", false)), detail);
}

pub(crate) fn draw_qi(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let Some(s) = &app.dash.qi else {
        let block = panel(t, "qi coins", true);
        let inner = block.inner(area);
        f.render_widget(block, area);
        empty_state(f, inner, t, spinner(), "Your coin purse hasn't been counted yet.", &[("R", "scan now")]);
        return;
    };
    let [summary, rest] = Layout::vertical([Constraint::Length(6), Constraint::Min(5)]).areas(area);
    // Stacked balance bar: spendable · locked · reserved.
    let block = panel(t, "qi balance", false);
    let inner = block.inner(summary);
    f.render_widget(block, summary);
    let total = u128::try_from(s.balance.total).unwrap_or(0).max(1) as f64;
    let part = |v: U256| (u128::try_from(v).unwrap_or(0) as f64 / total * f64::from(inner.width)).round() as usize;
    let (sp, lk, rs) = (part(s.balance.spendable), part(s.balance.locked), part(s.balance.reserved));
    let rest_w = (inner.width as usize).saturating_sub(sp + lk + rs);
    let legend = Line::from(vec![
        super::super::images::native_span(app, t, "qi"),
        Span::raw(" "),
        Span::styled("■ ", Style::default().fg(t.qi)),
        Span::styled(format!("spendable {}   ", qi(s.balance.spendable)), t.strong_style()),
        Span::styled("■ ", Style::default().fg(t.pending)),
        Span::styled(format!("locked {}   ", qi(s.balance.locked)), t.text_style()),
        Span::styled("■ ", Style::default().fg(t.attention)),
        Span::styled(format!("reserved {}", qi(s.balance.reserved)), t.text_style()),
        Span::styled(format!("   · checkpoint #{}", amount::group_thousands(&s.checkpoint_height.unwrap_or(0).to_string())), t.dim_style()),
    ]);
    let bar = Line::from(vec![
        Span::styled("█".repeat(sp), Style::default().fg(t.qi)),
        Span::styled("█".repeat(lk), Style::default().fg(t.pending)),
        Span::styled("█".repeat(rs), Style::default().fg(t.attention)),
        Span::styled("░".repeat(rest_w), t.dim_style()),
    ]);
    // Cash drawer: a slot per denomination held, stacked like notes in a till. The stack height
    // shows how many of that coin there are, so a wallet that needs consolidating *looks* like one:
    // tall stacks of small change on the left, a few big notes on the right.
    let values = wallet_core::sdk::consensus::Denomination::VALUES;
    let mut held = vec![0u64; values.len()];
    for c in &s.coins {
        if let Some(n) = held.get_mut(c.denomination as usize) {
            *n += 1;
        }
    }
    let stack = |n: u64| match n {
        0 => ' ',
        1 => '▁',
        2 => '▂',
        3..=4 => '▃',
        5..=8 => '▄',
        9..=16 => '▅',
        17..=32 => '▆',
        33..=64 => '▇',
        _ => '█',
    };
    let mut tray = vec![Span::styled("drawer ", t.dim_style())];
    let mut stacks = vec![Span::styled("       ", t.dim_style())];
    for (i, n) in held.iter().enumerate().filter(|(_, n)| **n > 0) {
        let lit = app.fx.drawer_flash.get(&(i as u8)).and_then(|s| super::super::edge::flash_bg(t, t.qi, s.elapsed().as_millis()));
        let bg = lit.or_else(|| super::super::edge::tint(t, t.qi, 0.22)).unwrap_or(t.raised);
        // A coin that landed since Qi was last looked at is marked until it is (every motion
        // level: the flash above is only the moving half of the news).
        let fresh = if app.fx.drawer_new.contains(&(i as u8)) { "•" } else { " " };
        let label = format!("{fresh}{} ×{n} ", super::super::num::qi(U256::from(values[i])));
        // Small coins cost the most fee to spend, so they read in the attention color once a
        // stack is deep enough to be worth consolidating.
        let heavy = i <= 6 && *n >= 8;
        let fg = if heavy { t.attention } else { t.qi };
        stacks.push(Span::styled(format!("{:^width$}", stack(*n), width = label.chars().count()), Style::default().fg(fg).bg(bg)));
        stacks.push(Span::raw(" "));
        tray.push(Span::styled(label, t.strong_style().bg(bg)));
        tray.push(Span::raw(" "));
    }
    if held.iter().all(|n| *n == 0) {
        tray.push(Span::styled("empty · coins arrive as fixed denominations", t.dim_style()));
    }
    // Spending many small coins costs more fee than spending a few big ones, so say when it is
    // worth tidying — and which key does it.
    let small: u64 = held.iter().take(7).sum();
    if small >= 8 {
        tray.push(Span::styled(format!("  {small} small coins — A aggregates them"), Style::default().fg(t.attention)));
    }
    f.render_widget(Paragraph::new(vec![legend, bar, Line::from(stacks), Line::from(tray)]), inner);

    let wide = rest.width >= 110;
    let [coins_area, side] = if wide {
        Layout::horizontal([Constraint::Min(60), Constraint::Length(46)]).areas(rest)
    } else {
        Layout::horizontal([Constraint::Min(40), Constraint::Length(0)]).areas(rest)
    };
    let title = if app.nav.jump_pending.is_some() { "coins · press a label" } else { "coins" };
    let block = panel(t, title, true);
    let inner = block.inner(coins_area);
    f.render_widget(block, coins_area);
    if s.coins.is_empty() {
        empty(
            f,
            inner,
            t,
            Icon::Coins,
            "No Qi yet. Coins arrive as fixed denominations, like cash.",
            &[("r", "receive"), ("C", "convert from QUAI")],
        );
    } else {
        let body = Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner };
        let offset = app.list_window(app.main_list(), app.nav.selected, s.coins.len(), body.height as usize);
        app.input.hits.borrow_mut().rows(app.main_list(), body, offset, s.coins.len(), |i| s.coins.get(i).map(|c| c.outpoint.clone()));
        let head = app.dash.health.as_ref().map(|h| h.height).unwrap_or(0);
        let rows: Vec<Row> = s
            .coins
            .iter()
            .enumerate()
            .skip(offset)
            .take(inner.height.saturating_sub(1) as usize)
            .map(|(i, c)| {
                let state = if c.reserved {
                    Span::styled(format!("{} reserved", t.icon(Icon::InFlight)), Style::default().fg(t.attention))
                } else if !c.unlock_height.is_zero() && U256::from(head) < c.unlock_height {
                    Span::styled(format!("{} locked", t.icon(Icon::Locked)), Style::default().fg(t.pending))
                } else {
                    Span::styled(t.icon(Icon::Ok), Style::default().fg(t.ok))
                };
                let row = Row::new(vec![
                    Cell::from(Span::styled(app::jump_label(i - offset).to_string(), jump_style(app, t))),
                    Cell::from(Span::styled("◉", Style::default().fg(t.qi))),
                    Cell::from(
                        Line::from(Span::styled(amount::group_thousands(&amount::qi(U256::from(c.qits))), t.strong_style()))
                            .alignment(Alignment::Right),
                    ),
                    Cell::from(state),
                    Cell::from(c.label.clone().unwrap_or_else(|| c.origin.clone())),
                    Cell::from(Span::styled(short_address(&c.address), t.dim_style())),
                ]);
                if i == app.nav.selected { row.style(t.selected()) } else { row }
            })
            .collect();
        let table = Table::new(
            rows,
            [
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(12),
                Constraint::Length(10),
                Constraint::Min(12),
                Constraint::Length(13),
            ],
        )
        .header(Row::new(["", "", "Qi", "state", "origin", "address"]).style(t.dim_style()));
        f.render_widget(table, inner);
    }
    if side.width == 0 {
        return;
    }
    let [hist_area, addr_area] = Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(side);
    // Denomination histogram: where the cash sits.
    let values = wallet_core::sdk::consensus::Denomination::VALUES;
    let mut counts = vec![0u64; values.len()];
    for c in &s.coins {
        if let Some(n) = counts.get_mut(c.denomination as usize) {
            *n += 1;
        }
    }
    let max = counts.iter().copied().max().unwrap_or(1).max(1);
    let block = panel(t, "denominations", false);
    let inner = block.inner(hist_area);
    f.render_widget(block, hist_area);
    let bar_w = inner.width.saturating_sub(22) as u64;
    let lines: Vec<Line> = counts
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, n)| **n > 0)
        .take(inner.height as usize)
        .map(|(i, n)| {
            let cells = (n * bar_w * 8).div_ceil(max);
            let full = (cells / 8) as usize;
            let frac = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"][(cells % 8) as usize];
            Line::from(vec![
                Span::styled(format!("{:>8} ", super::super::num::qi(U256::from(values[i]))), t.dim_style()),
                Span::styled(format!("{}{frac}", "█".repeat(full)), Style::default().fg(t.qi)),
                Span::styled(format!(" ×{n}"), t.strong_style()),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
    let lines: Vec<Line> = app
        .dash
        .qi_addresses
        .iter()
        .rev()
        .take(addr_area.height.saturating_sub(2) as usize)
        .map(|(i, a, l)| {
            Line::from(vec![
                Span::styled(format!("#{i:<7} "), t.dim_style()),
                Span::raw(short_address(a)),
                Span::styled(format!(" {}", l.clone().unwrap_or_default()), Style::default().fg(t.qi)),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines).block(panel(t, "receive & mining addresses", false)), addr_area);
}

pub(crate) fn draw_payments(f: &mut Frame, app: &App, t: &Theme, area: Rect, channels: bool) {
    let code = app.meta.as_ref().and_then(|m| m.payment_code.clone());
    let inner_w = area.width.saturating_sub(4).max(1) as usize;
    let code_rows = code.as_ref().map_or(1, |c| c.chars().count().div_ceil(inner_w));
    let [code_area, list_row] = Layout::vertical([Constraint::Length(code_rows as u16 + 3), Constraint::Min(6)]).areas(area);
    let lines = match code {
        Some(c) => vec![
            Line::from(Span::styled(c, t.strong_style().fg(t.qi))),
            Line::from(Span::styled(
                "Your payment code: share it to get paid privately. r QR · y copy on a row · d scan mailbox now",
                t.dim_style(),
            )),
        ],
        None => vec![Line::from(Span::styled("This wallet has no payment code (import a recovery phrase to get one).", t.dim_style()))],
    };
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).block(panel(t, "you", false)), code_area);

    let wide = list_row.width >= 110;
    let [list_area, detail_area] = if wide {
        Layout::horizontal([Constraint::Min(50), Constraint::Length(48)]).areas(list_row)
    } else {
        Layout::horizontal([Constraint::Min(40), Constraint::Length(0)]).areas(list_row)
    };
    let contacts_focused = !channels;
    let (contacts_area, channels_area) = if channels { (Rect::default(), list_area) } else { (list_area, Rect::default()) };
    let block = panel(t, &format!("contacts · {}", app.dash.contacts.len()), contacts_focused);
    let inner = block.inner(contacts_area);
    if !channels {
        f.render_widget(block, contacts_area);
    }
    if channels {
    } else if app.dash.contacts.is_empty() {
        empty(f, inner, t, Icon::People, "No contacts yet. Save people by address, payment code, or both.", &[("a", "add contact")]);
    } else {
        let n = app.dash.contacts.len();
        let body = Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner };
        let offset = app.list_window(app.main_list(), app.nav.selected, n, body.height as usize);
        app.input.hits.borrow_mut().rows(app.main_list(), body, offset, n, |i| app.dash.contacts.get(i).map(|c| c.name.clone()));
        let rows: Vec<Row> = app
            .dash
            .contacts
            .iter()
            .enumerate()
            .skip(offset)
            .take(body.height as usize)
            .map(|(i, c)| {
                let ledger = c
                    .address
                    .as_deref()
                    .and_then(|a| wallet_core::registry::parse_any_address(a).ok())
                    .map(|a| if a.ledger() == wallet_core::sdk::Ledger::Qi { ("Qi", t.qi) } else { ("QUAI", t.quai) });
                let row = Row::new(vec![
                    Cell::from(Span::styled(app::jump_label(i - offset).to_string(), jump_style(app, t))),
                    Cell::from(Span::styled(c.name.clone(), t.strong_style())),
                    Cell::from(match (&c.address, ledger) {
                        (Some(a), Some((label, color))) => {
                            Line::from(vec![Span::styled(format!("{label:<4} "), Style::default().fg(color)), Span::raw(short_address(a))])
                        }
                        _ => Line::from(Span::styled("—", t.dim_style())),
                    }),
                    Cell::from(match &c.payment_code {
                        Some(code) => Line::from(Span::styled(short_code(code), Style::default().fg(t.qi))),
                        None => Line::from(Span::styled("—", t.dim_style())),
                    }),
                    Cell::from(Span::styled(truncate(&c.note, 24), t.dim_style())),
                ]);
                if contacts_focused && i == app.nav.selected { row.style(t.selected()) } else { row }
            })
            .collect();
        f.render_widget(
            Table::new(
                rows,
                [Constraint::Length(1), Constraint::Length(16), Constraint::Length(19), Constraint::Length(16), Constraint::Min(8)],
            )
            .column_spacing(2)
            .header(Row::new(["", "name", "address", "payment code", "note"]).style(t.dim_style())),
            inner,
        );
    }
    if detail_area.width > 0 {
        let block = panel(t, "details", false);
        let inner = block.inner(detail_area);
        f.render_widget(block, detail_area);
        let mut lines: Vec<Line> = Vec::new();
        let label = |k: &str| Line::from(Span::styled(k.to_string(), t.dim_style()));
        let selected_contact = if !channels { app.dash.contacts.get(app.nav.selected).cloned() } else { None };
        let selected_peer = if channels { app.channel_peer() } else { None };
        let selected_offer = if channels { app.channel_offer() } else { None };
        if let Some(c) = &selected_contact {
            lines.push(Line::from(Span::styled(c.name.clone(), t.strong_style())));
            lines.push(Line::from(""));
            if let Some(a) = &c.address {
                lines.push(label("address"));
                lines.push(Line::from(Span::styled(a.clone(), Style::default().fg(t.link))));
            }
            if let Some(code) = &c.payment_code {
                lines.push(label("payment code"));
                lines.push(Line::from(Span::styled(code.clone(), Style::default().fg(t.qi))));
                if let Some(p) = app.dash.peers.iter().find(|p| p.code == *code) {
                    lines.push(Line::from(Span::styled(
                        format!("channel · ↘ {} received · ↗ {} sent", p.receive_addresses, p.send_addresses),
                        t.dim_style(),
                    )));
                }
            }
            if !c.note.is_empty() {
                lines.push(label("note"));
                lines.push(Line::from(c.note.clone()));
            }
            // Token and NFT transfers with this person (explorer activity).
            if let Some(addr) = c.address.as_deref() {
                let with: Vec<&wallet_core::appdb::Activity> = app
                    .dash
                    .activity
                    .iter()
                    .filter(|a| a.detail.counterparty().as_str().is_some_and(|cp| cp.eq_ignore_ascii_case(addr)))
                    .take(4)
                    .collect();
                if !with.is_empty() {
                    lines.push(label("transfers"));
                    for a in with {
                        let arrow = if a.direction == "in" { "↘" } else { "↗" };
                        lines.push(Line::from(vec![
                            Span::styled(format!("{arrow} "), t.dim_style()),
                            Span::raw(super::super::views::activity_text(a)),
                        ]));
                    }
                }
            }
            lines.push(Line::from(""));
            let mut hints = vec![];
            if c.payment_code.is_some() || c.address.as_deref().is_some_and(|a| a.starts_with("0x")) {
                hints.push(("enter", "pay"));
            }
            if c.address.is_some() {
                hints.push(("Q", "send QUAI"));
            }
            if c.payment_code.is_some() {
                hints.push(("n", "notify"));
            }
            hints.extend([("e", "edit"), ("y", "copy"), ("x", "remove")]);
            for (k, v) in hints {
                lines.push(Line::from(vec![Span::styled(format!("{k:>5}  "), t.strong_style().fg(t.focus)), Span::raw(v)]));
            }
        } else if let Some(o) = selected_offer {
            lines.push(Line::from(Span::styled("channel offer", t.strong_style().fg(t.attention))));
            lines.push(Line::from(""));
            lines.push(label("payment code"));
            lines.push(Line::from(Span::styled(o.code.clone(), Style::default().fg(t.qi))));
            lines.push(Line::from(Span::styled(format!("{} Qi waiting (at least)", super::super::num::qi(o.found)), t.dim_style())));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Announced through the mailbox. Anyone can announce, so it is not a channel until you accept it.",
                t.dim_style(),
            )));
            lines.push(Line::from(""));
            for (k, v) in [("enter", "accept"), ("x", "decline"), ("y", "copy")] {
                lines.push(Line::from(vec![Span::styled(format!("{k:>5}  "), t.strong_style().fg(t.focus)), Span::raw(v)]));
            }
        } else if let Some(p) = selected_peer {
            lines.push(Line::from(Span::styled(p.contact.clone().unwrap_or_else(|| "unsaved sender".into()), t.strong_style())));
            lines.push(Line::from(""));
            lines.push(label("payment code"));
            lines.push(Line::from(Span::styled(p.code.clone(), Style::default().fg(t.qi))));
            lines
                .push(Line::from(Span::styled(format!("↘ {} received · ↗ {} sent", p.receive_addresses, p.send_addresses), t.dim_style())));
            lines.push(Line::from(""));
            let save = if p.contact.is_some() { "edit contact" } else { "save as contact" };
            for (k, v) in [("enter", "pay Qi"), ("a", save), ("S", "rescan"), ("n", "notify"), ("y", "copy")] {
                lines.push(Line::from(vec![Span::styled(format!("{k:>5}  "), t.strong_style().fg(t.focus)), Span::raw(v)]));
            }
        } else {
            lines.push(Line::from(Span::styled("select a contact or channel", t.dim_style())));
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }

    if !channels {
        return;
    }
    let channels_focused = true;
    let offered = if app.dash.offers.is_empty() { String::new() } else { format!(" · {} offered", app.dash.offers.len()) };
    let block = panel(t, &format!("payment channels · {}{offered}", app.dash.peers.len()), channels_focused);
    let inner = block.inner(channels_area);
    f.render_widget(block, channels_area);
    if !app.dash.unlocked {
        empty(f, inner, t, Icon::Lock, "Unlock to see payment channels.", &[]);
    } else if app.dash.peers.is_empty() && app.dash.offers.is_empty() {
        empty_state(
            f,
            inner,
            t,
            "@",
            "No channels yet. A sender who announces a channel and pays you is offered here, to accept or decline.",
            &[("d", "scan mailbox"), ("p", "add a peer")],
        );
    } else {
        let offers = app.dash.offers.len();
        let n = offers + app.dash.peers.len();
        let body = Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner };
        let offset = app.list_window(app.main_list(), app.nav.selected, n, body.height as usize);
        app.input.hits.borrow_mut().rows(app.main_list(), body, offset, n, |i| app.row_key(app.main_list(), i));
        let offer_rows = app.dash.offers.iter().enumerate().map(|(i, o)| {
            let row = Row::new(vec![
                Cell::from(Span::styled(app::jump_label(i.wrapping_sub(offset)).to_string(), jump_style(app, t))),
                Cell::from(Span::styled(short_code(&o.code), Style::default().fg(t.qi))),
                Cell::from(Span::styled("offered · s accepts, x declines", Style::default().fg(t.attention))),
                Cell::from(format!("{} Qi", super::super::num::qi(o.found))),
                Cell::from(""),
            ]);
            if channels_focused && i == app.nav.selected { row.style(t.selected()) } else { row }
        });
        let rows: Vec<Row> = offer_rows
            .chain(app.dash.peers.iter().enumerate().map(|(i, p)| (i + offers, p)).map(|(i, p)| {
                let who = match &p.contact {
                    Some(name) => Span::styled(name.clone(), t.strong_style()),
                    None => Span::styled("unsaved · a to save", Style::default().fg(t.attention)),
                };
                let row = Row::new(vec![
                    Cell::from(Span::styled(app::jump_label(i.wrapping_sub(offset)).to_string(), jump_style(app, t))),
                    Cell::from(Span::styled(short_code(&p.code), Style::default().fg(t.qi))),
                    Cell::from(who),
                    Cell::from(format!("↘ {}", p.receive_addresses)),
                    Cell::from(format!("↗ {}", p.send_addresses)),
                ]);
                if channels_focused && i == app.nav.selected { row.style(t.selected()) } else { row }
            }))
            .skip(offset)
            .take(body.height as usize)
            .collect();
        f.render_widget(
            Table::new(
                rows,
                [Constraint::Length(1), Constraint::Length(16), Constraint::Min(20), Constraint::Length(10), Constraint::Length(8)],
            )
            .column_spacing(2)
            .header(Row::new(["", "payment code", "contact", "recv", "sent"]).style(t.dim_style())),
            inner,
        );
    }
}

/// The wallet's time locks, a line each, under the accounts. Converted coins wait here with a
/// countdown until they can be spent; nothing to do but wait.
fn draw_locks(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let agrees = match &app.eco.feeds.lockups {
        Some(Ok(total)) if (*total == 0) == app.dash.locks.iter().all(|l| l.unlocked) => {
            &format!(" · {} explorer agrees", t.icon(Icon::Ok))
        }
        Some(Ok(_)) => " · ! explorer differs",
        _ => "",
    };
    let block = panel(t, &format!("time locks{agrees}"), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if app.dash.locks.is_empty() {
        let line = Line::from(vec![
            Span::styled(t.lead(Icon::Ok), Style::default().fg(t.ok)),
            Span::styled("Nothing time-locked: everything is spendable. Converted coins wait here until they unlock.", t.dim_style()),
        ]);
        f.render_widget(Paragraph::new(line), inner);
        return;
    }
    // Exact countdowns; the lock start height isn't known for every source.
    let shown = inner.height as usize;
    let amounts = super::super::num::align(&app.dash.locks.iter().take(shown).map(|l| l.amount.clone()).collect::<Vec<_>>());
    let amount_w = amounts.iter().map(|a| a.chars().count()).max().unwrap_or(0);
    for (i, l) in app.dash.locks.iter().enumerate().take(shown) {
        let row = Rect::new(inner.x, inner.y + i as u16, inner.width, 1);
        let when = if l.unlocked {
            Span::styled(format!("{} spendable", t.icon(Icon::Ok)), Style::default().fg(t.ok))
        } else {
            let at = l
                .unlock_height
                .map(|h| format!("{} unlocks #{}", t.icon(Icon::Locked), amount::group_thousands(&h.to_string())))
                .unwrap_or_else(|| format!("{} settling", t.icon(Icon::InFlight)));
            // The block count goes first when the row is short of room; the time says it too.
            let remaining =
                l.blocks_remaining.filter(|_| inner.width >= 90).map(|b| format!(" · {}", amount::count(b, "block"))).unwrap_or_default();
            let eta = l.eta_secs.map(|s| format!(" · ~{}", human_duration(s))).unwrap_or_default();
            Span::styled(format!("{at}{remaining}{eta}"), Style::default().fg(t.pending))
        };
        let color = if l.asset.eq_ignore_ascii_case("QI") { t.qi } else { t.quai };
        let mut line = Line::from(vec![
            Span::styled("▌", Style::default().fg(color)),
            Span::styled(format!(" {:>amount_w$} {:<5} ", amounts[i], super::super::num::unit(&l.asset)), t.strong_style()),
            when,
        ]);
        // Where it came from, in the room that is left, ending "…" rather than mid-word.
        let room = (inner.width as usize).saturating_sub(line.width() + 2);
        if room >= 6 {
            line.spans.push(Span::styled(format!("  {}", truncate(&l.source, room)), t.dim_style()));
        }
        f.render_widget(Paragraph::new(line), row);
    }
    if app.dash.locks.len() > shown {
        let more = format!(" {} more ", app.dash.locks.len() - shown);
        let w = more.chars().count() as u16;
        f.render_widget(
            Paragraph::new(Span::styled(more, t.dim_style())),
            Rect::new(area.right().saturating_sub(w + 2), area.bottom() - 1, w, 1),
        );
    }
}

/// The selected account, beside the list at `Wide`: its address in groups of four to check
/// against, its numbers, and a QR code to receive on it.
fn draw_account_inspector(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let Some(a) = app.dash.accounts.get(app.nav.selected) else { return };
    let block = panel(t, &format!("account · {}", a.label), false);
    let frame_h = area.height.saturating_sub(block.inner(area).height);
    let inner = block.inner(area);
    // A QR code to receive on it: beside the details when the panel is wide, under them when it
    // is tall, and left out when it would not fit whole.
    let qr = (app.term.caps.tier != Tier::Text && !app.term.plain).then(|| super::super::terminal::qr_modules(&a.address, 2)).flatten();
    let (qr_w, qr_h) = qr.as_ref().map_or((0, 0), |(size, _)| (*size as u16, size.div_ceil(2) as u16));
    let kv = |k: &str, v: Vec<Span<'static>>| super::super::widgets::kv(t, k, v);
    let mut lines = vec![kv("balance", vec![Span::styled(format!("{} QUAI", q(a.balance)), t.strong_style().fg(t.quai))])];
    if !a.locked.is_zero() {
        lines.push(kv(
            "locked",
            vec![Span::styled(format!("{} {} QUAI", t.icon(Icon::Locked), q(a.locked)), Style::default().fg(t.pending))],
        ));
    }
    lines.push(kv("sent", vec![Span::styled(format!("{} (the nonce)", amount::count(a.nonce, "transaction")), t.text_style())]));
    if let Some(i) = a.hd_index {
        lines.push(kv("derived", vec![Span::styled(format!("index {i} of the phrase"), t.text_style())]));
    }
    // In groups of four: on one line when there is room, otherwise `0x` and five groups, then
    // the other five under them.
    let details_w = lines.iter().map(|l| l.width() as u16).max().unwrap_or(0).max(28);
    let beside = qr.is_some() && inner.width >= details_w + 2 + qr_w && inner.height >= qr_h;
    let text_w = if beside { inner.width - qr_w - 2 } else { inner.width };
    let mut grouped = super::super::widgets::address(t, &a.address, t.text_style());
    let mut head = if text_w >= 54 {
        vec![Line::from(grouped)]
    } else {
        let second: Vec<Span> = grouped.split_off(6.min(grouped.len()));
        vec![Line::from(grouped), Line::from([vec![Span::raw("  ")], second].concat())]
    };
    head.push(Line::from(""));
    head.append(&mut lines);
    let lines = head;
    let text_h = lines.len() as u16;
    let below = qr.is_some() && !beside && inner.height >= text_h + 1 + qr_h;
    // The panel is as tall as what it holds.
    let content_h = if beside {
        text_h.max(qr_h)
    } else if below {
        text_h + 1 + qr_h
    } else {
        text_h
    };
    let rect = Rect { height: (content_h + frame_h).min(area.height), ..area };
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    f.render_widget(Paragraph::new(lines), Rect { width: text_w, ..inner });
    if let Some((size, grid)) = qr {
        let at = if beside {
            Some(Rect { x: inner.right() - qr_w, width: qr_w, height: qr_h, ..inner })
        } else if below {
            Some(Rect { y: inner.y + text_h + 1, height: qr_h, ..inner })
        } else {
            None
        };
        if let Some(at) = at {
            paint_qr(f.buffer_mut(), at, size, &grid);
        }
    }
}

/// Right-aligned samples scaled between their min and max so small changes stay visible.
pub(crate) fn scaled(data: &[u64], width: u16) -> Vec<u64> {
    let tail: Vec<u64> = data.iter().rev().take(width as usize).rev().copied().collect();
    let min = tail.iter().copied().min().unwrap_or(0);
    let mut out = vec![0u64; (width as usize).saturating_sub(tail.len())];
    out.extend(tail.iter().map(|v| v - min + 1));
    out
}

/// A series resampled to exactly `width` bars, so a 48-hour history fills its panel rather than
/// sitting in one corner of it. Bars are measured up from a little under the lowest value — the
/// shape is what a sparkline is for, and from zero every hour of a busy chain looks the same.
pub(crate) fn stretched(values: &[f64], width: u16) -> Vec<u64> {
    let width = width as usize;
    if values.is_empty() || width == 0 {
        return vec![0; width];
    }
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let floor = min - (max - min).max(min.abs() * 0.05) * 0.25;
    let span = (max - floor).max(f64::MIN_POSITIVE);
    (0..width)
        .map(|x| {
            let v = values[(x * values.len() / width).min(values.len() - 1)];
            (((v - floor) / span) * 100.0).round().max(1.0) as u64
        })
        .collect()
}

/// `100.7M`, `7,651`: counts on a chart title, short enough to leave room for the chart.
pub(crate) fn count_text(n: u64) -> String {
    match n {
        n if n >= 1_000_000_000 => format!("{:.2}B", n as f64 / 1e9),
        n if n >= 10_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n => amount::group_thousands(&n.to_string()),
    }
}

/// `38,567 gwei`, from gwei.
pub(crate) fn gwei_text(gwei: f64) -> String {
    format!("{} gwei", amount::group_thousands(&format!("{:.0}", gwei.max(0.0))))
}

/// The chain as the explorer sees it, beside the node's own health: totals, the last hour, and
/// where the figures came from and how old they are.
pub(crate) fn draw_chain(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let kv = |k: &str, v: Vec<Span<'static>>| {
        let mut spans = vec![Span::styled(format!("{k:<w$}", w = super::super::widgets::KV_LABEL), t.dim_style())];
        spans.extend(v);
        Line::from(spans)
    };
    let lines = match app.eco.feeds.chain_stats.shown() {
        None => vec![Line::from(Span::styled(format!("{} reading network statistics…", spinner()), t.dim_style()))],
        Some(Err(e)) => vec![
            Line::from(Span::styled(format!("{} {}", t.icon(Icon::Info), truncate(&app::friendly_error(e), 80)), t.dim_style())),
            Line::from(Span::styled("Node health on the left is read from the node itself.", t.dim_style())),
        ],
        Some(Ok(s)) => {
            let last = s.hours.last();
            let mut lines = vec![
                kv("avg block", vec![Span::raw(s.avg_block_secs.map(|b| format!("{b:.2} s")).unwrap_or_else(|| "—".into()))]),
                kv(
                    "transactions",
                    vec![
                        Span::styled(
                            s.total_transactions.map(|n| amount::group_thousands(&n.to_string())).unwrap_or_else(|| "—".into()),
                            t.strong_style(),
                        ),
                        Span::styled(
                            last.map(|h| format!(" · {} last hour", amount::group_thousands(&h.transactions.to_string())))
                                .unwrap_or_default(),
                            t.dim_style(),
                        ),
                    ],
                ),
            ];
            if let Some(h) = last {
                let cross = h.transactions.saturating_sub(h.quai_transactions + h.qi_transactions);
                lines.push(kv(
                    "",
                    vec![Span::styled(
                        format!(
                            "{} QUAI · {} Qi · {} cross-zone",
                            amount::group_thousands(&h.quai_transactions.to_string()),
                            amount::group_thousands(&h.qi_transactions.to_string()),
                            amount::group_thousands(&cross.to_string())
                        ),
                        t.dim_style(),
                    )],
                ));
            }
            let addresses = match (s.quai_addresses, s.qi_addresses) {
                (Some(q), Some(i)) => {
                    format!("{} QUAI · {} Qi", amount::group_thousands(&q.to_string()), amount::group_thousands(&i.to_string()))
                }
                (Some(q), None) => amount::group_thousands(&q.to_string()),
                _ => "—".into(),
            };
            lines.push(kv("addresses", vec![Span::raw(addresses)]));
            lines.push(kv(
                "block reward",
                vec![Span::raw(s.block_reward_quai.map(|r| format!("{r:.2} QUAI")).unwrap_or_else(|| "—".into()))],
            ));
            if let Some(g) = last.and_then(|h| h.avg_gas_price_gwei()) {
                lines.push(kv("gas paid", vec![Span::raw(gwei_text(g)), Span::styled(" avg, last hour", t.dim_style())]));
            }
            let observed = s.observed_at.min(wallet_core::registry::now());
            let when = match ago(observed) {
                now if now == "now" => "just now".to_string(),
                age => format!("as of {age} ago"),
            };
            lines.push(kv("source", vec![Span::styled(format!("explorer.qu.ai · {when}"), t.dim_style())]));
            lines
        }
    };
    f.render_widget(Paragraph::new(lines).block(panel(t, "chain", false)), area);
}

/// Hashrate, transactions and gas over time: three panels on one row.
pub(crate) fn draw_chain_charts(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let [hash, txs, gas] =
        Layout::horizontal([Constraint::Percentage(36), Constraint::Percentage(32), Constraint::Percentage(32)]).areas(area);
    let stats = app.eco.feeds.chain_stats.value();
    // Hashrate: one row per algorithm, each on its own scale — they differ by six orders of
    // magnitude, so one shared axis would draw two flat lines and a wall.
    let block = panel(t, &format!("{}hashrate · 24h", t.lead(Icon::Mining)), false);
    let inner = block.inner(hash);
    f.render_widget(block, hash);
    match stats {
        Some(s) if !s.hashrate_history.is_empty() || s.hashrate.sha > 0.0 => {
            type Pick = fn(&wallet_core::chainstats::Hashrates) -> f64;
            let algos: [(&str, Pick, Color); 3] =
                [("SHA", |h| h.sha, t.chart[0]), ("Scrypt", |h| h.scrypt, t.chart[2]), ("KawPoW", |h| h.kawpow, t.chart[4])];
            for (i, (name, pick, colour)) in algos.iter().enumerate() {
                let y = inner.y + (i as u16) * 2;
                if y >= inner.bottom() {
                    break;
                }
                let label = format!("{name:<7}{:>11} ", wallet_core::chainstats::hashrate_text(pick(&s.hashrate)));
                let label_w = (label.chars().count() as u16).min(inner.width);
                f.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(format!("{name:<7}"), t.dim_style()),
                        Span::styled(label[7..].to_string(), t.strong_style()),
                    ])),
                    Rect { x: inner.x, y, width: label_w, height: 1 },
                );
                let spark = Rect { x: inner.x + label_w, y, width: inner.width.saturating_sub(label_w), height: 1 };
                let series: Vec<f64> = s.hashrate_history.iter().map(|(_, h)| pick(h)).collect();
                f.render_widget(Sparkline::default().data(stretched(&series, spark.width)).style(Style::default().fg(*colour)), spark);
            }
        }
        _ => chart_placeholder(f, app, t, inner),
    }
    // Transactions per hour, with the running total in the title.
    let total = stats.and_then(|s| s.total_transactions).map(|n| format!(" · total {}", count_text(n))).unwrap_or_default();
    let last = stats.and_then(|s| s.hours.last()).map(|h| format!(" · {}/h", count_text(h.transactions))).unwrap_or_default();
    let block = panel(t, &format!("transactions{last}{total}"), false);
    let inner = block.inner(txs);
    f.render_widget(block, txs);
    match stats.filter(|s| !s.hours.is_empty()) {
        Some(s) => {
            let series: Vec<f64> = s.hours.iter().map(|h| h.transactions as f64).collect();
            spark_with_axis(f, app, t, inner, &series, t.ok, s.hours.len());
        }
        None => chart_placeholder(f, app, t, inner),
    }
    // Gas: the node's price now in the title, what people actually paid each hour below.
    let node = app.dash.health.as_ref().and_then(|h| h.gas_price.parse::<f64>().ok()).filter(|p| *p > 0.0).map(|wei| wei / 1e9);
    let title = match node {
        Some(g) => format!("gas · node {} now", gwei_text(g)),
        None => "gas · paid per hour".into(),
    };
    let block = panel(t, &title, false);
    let inner = block.inner(gas);
    f.render_widget(block, gas);
    let paid: Vec<f64> = stats.map(|s| s.hours.iter().filter_map(|h| h.avg_gas_price_gwei()).collect()).unwrap_or_default();
    if paid.is_empty() {
        chart_placeholder(f, app, t, inner);
    } else {
        spark_with_axis(f, app, t, inner, &paid, t.attention, paid.len());
    }
}

/// A sparkline over all but the last row, and under it how far back it reaches and its range.
pub(crate) fn spark_with_axis(f: &mut Frame, app: &App, t: &Theme, inner: Rect, series: &[f64], colour: Color, hours: usize) {
    if inner.height < 2 || inner.width < 8 {
        return;
    }
    let chart = Rect { height: inner.height - 1, ..inner };
    f.render_widget(Sparkline::default().data(stretched(series, chart.width)).style(Style::default().fg(colour)), chart);
    super::super::edge::ramp_bars(app, f.buffer_mut(), chart, colour, t);
    let (lo, hi) = series.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), v| (a.min(*v), b.max(*v)));
    let range = format!("{} – {}", count_text(lo.max(0.0) as u64), count_text(hi.max(0.0) as u64));
    let left = format!("{hours}h ago");
    let gap = (inner.width as usize).saturating_sub(left.len() + range.len() + 4);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(left, t.dim_style()),
            Span::styled(format!("{:gap$}", ""), t.dim_style()),
            Span::styled(range, t.dim_style()),
            Span::styled("  now", t.dim_style()),
        ])),
        Rect { y: inner.bottom() - 1, height: 1, ..inner },
    );
}

/// What a chart shows before its data arrives, or when there is none for this network.
pub(crate) fn chart_placeholder(f: &mut Frame, app: &App, t: &Theme, inner: Rect) {
    let text = match app.eco.feeds.chain_stats.shown() {
        None => format!("{} loading…", spinner()),
        Some(Err(_)) => format!("{} no statistics for this network", t.icon(Icon::Info)),
        Some(Ok(_)) => format!("{} no history yet", t.icon(Icon::Info)),
    };
    f.render_widget(Paragraph::new(Span::styled(text, t.dim_style())), inner);
}

pub(crate) fn draw_node(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    // Node health beside the chain's own figures, the node's two sparklines, the chain's history,
    // then the network list. The history row is dropped before the list when the terminal is short.
    let history = if area.height >= 30 { 7 } else { 0 };
    let [top, charts, chain_charts, nets] =
        Layout::vertical([Constraint::Length(10), Constraint::Length(5), Constraint::Length(history), Constraint::Min(4)]).areas(area);
    let (info, chain) = if top.width >= 110 {
        let [a, b] = Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)]).areas(top);
        (a, Some(b))
    } else {
        (top, None)
    };
    if let Some(chain) = chain {
        draw_chain(f, app, t, chain);
    }
    if history > 0 {
        draw_chain_charts(f, app, t, chain_charts);
    }
    let mut lines = Vec::new();
    let kv = |k: &str, v: String| super::super::widgets::kv(t, k, vec![Span::raw(v)]);
    match (&app.dash.health, &app.dash.node_error) {
        (_, Some(e)) => {
            lines.push(Line::from(Span::styled(
                format!("{} {}", t.icon(Icon::Danger), app::friendly_error(e)),
                Style::default().fg(t.danger),
            )));
            lines.push(Line::from(Span::styled(
                "Retrying automatically. Check the RPC URL with `quai-terminal network list`.",
                t.dim_style(),
            )));
        }
        (Some(h), None) => {
            lines.push(kv("network", format!("{} ({})", app.dash.network_name, app.dash.network_id)));
            lines.push(Line::from(vec![
                Span::styled(format!("{:<w$}", "identity", w = super::super::widgets::KV_LABEL), t.dim_style()),
                Span::styled(
                    if h.identity_ok {
                        format!("{} chain id and genesis match", t.icon(Icon::Ok))
                    } else {
                        format!("{} MISMATCH — do not transact", t.icon(Icon::Danger))
                    },
                    Style::default().fg(if h.identity_ok { t.ok } else { t.danger }),
                ),
            ]));
            lines.push(kv("chain id", h.chain_id.clone()));
            lines.push(kv("genesis", h.genesis.clone()));
            lines.push(kv("height", amount::group_thousands(&h.height.to_string())));
            // What `:poem` is about, in one still line: the smallest head hash seen this session.
            if let Some((hash, at)) = &app.fx.lowest_hash {
                let zeros = hash.trim_start_matches("0x").chars().take_while(|c| *c == '0').count();
                lines.push(kv(
                    "lowest hash",
                    format!(
                        "{} · #{} · {} leading zeros · this session's entropy minimum",
                        short_address(hash),
                        amount::group_thousands(&at.to_string()),
                        zeros
                    ),
                ));
            }
            lines.push(kv(
                "head age",
                h.head_age_secs.map(|s| if s < 60 { format!("{s}s") } else { human_duration(s) }).unwrap_or_else(|| "?".into()),
            ));
            lines.push(kv(
                "gas price",
                format!("{} gwei", amount::group_thousands(&amount::format_amount_short(h.gas_price.parse().unwrap_or_default(), 9, 3))),
            ));
            lines.push(kv("client", h.client_version.clone().unwrap_or_else(|| "—".into())));
            lines.push(kv(
                "monitoring",
                match app.config.monitor_endpoints.get(&app.dash.network_id) {
                    Some(m) => format!("{} · reads only (m to change)", m.rpc_url),
                    None => "main RPC (m to add a read-only endpoint)".into(),
                },
            ));
        }
        _ => lines.push(Line::from(Span::styled(format!("{} checking node…", spinner()), t.dim_style()))),
    }
    f.render_widget(Paragraph::new(lines).block(panel(t, "node health", false)), info);
    let [lat, blocks] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(charts);
    let hist = &app.dash.latency_history;
    let (min, max, last) =
        (hist.iter().min().copied().unwrap_or(0), hist.iter().max().copied().unwrap_or(0), hist.last().copied().unwrap_or(0));
    let block = panel(t, &format!("latency {last} ms · min {min} · max {max}"), false);
    let inner = block.inner(lat);
    f.render_widget(block, lat);
    f.render_widget(Sparkline::default().data(scaled(hist, inner.width)).style(Style::default().fg(t.link)), inner);
    super::super::edge::ramp_bars(app, f.buffer_mut(), inner, t.link, t);
    let deltas: Vec<u64> = app.dash.height_history.windows(2).map(|w| w[1].saturating_sub(w[0])).collect();
    let block = panel(t, &format!("blocks per refresh · last {}", deltas.last().copied().unwrap_or(0)), false);
    let inner = block.inner(blocks);
    f.render_widget(block, blocks);
    f.render_widget(Sparkline::default().data(scaled(&deltas, inner.width)).style(Style::default().fg(t.ok)), inner);
    super::super::edge::ramp_bars(app, f.buffer_mut(), inner, t.ok, t);
    let rows: Vec<Row> = app
        .dash
        .networks
        .iter()
        .enumerate()
        .map(|(i, (id, name))| {
            let active = *id == app.dash.network_id;
            let row = Row::new(vec![
                Cell::from(Span::styled(
                    t.icon(if active { Icon::On } else { Icon::Off }),
                    Style::default().fg(if active { t.ok } else { t.dim }),
                )),
                Cell::from(id.clone()),
                Cell::from(name.clone()),
                Cell::from(Span::styled(if id == "mainnet" { "real funds" } else { "" }, Style::default().fg(t.attention))),
            ]);
            if i == app.nav.selected { row.style(t.selected()) } else { row }
        })
        .collect();
    let block = panel(t, "networks · enter to switch", true);
    let inner = block.inner(nets);
    f.render_widget(block, nets);
    let n = rows.len();
    let offset = app.list_window(app.main_list(), app.nav.selected, n, inner.height as usize);
    app.input.hits.borrow_mut().rows(app.main_list(), inner, offset, n, |i| app.dash.networks.get(i).map(|(id, _)| id.clone()));
    let rows: Vec<Row> = rows.into_iter().skip(offset).collect();
    f.render_widget(Table::new(rows, [Constraint::Length(1), Constraint::Length(16), Constraint::Min(10), Constraint::Length(12)]), inner);
}

/// A feature switch: what it covers while on.
pub(crate) fn feature_value(t: &Theme, on: bool, covers: &str) -> String {
    if on { format!("{} on · {covers}", t.icon(Icon::On)) } else { format!("{} off", t.icon(Icon::Off)) }
}

pub(crate) fn draw_settings(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let c = &app.config;
    let (on, off) = (t.icon(Icon::On), t.icon(Icon::Off));
    let on_off = |b: bool| if b { format!("{on} on") } else { format!("{off} off") };
    let settings = app.settings_rows();
    let rows: Vec<(&str, Row)> = settings
        .iter()
        .enumerate()
        .map(|(i, (id, label))| {
            let value = match *id {
                "theme" => {
                    format!(
                        "{}  ›",
                        super::super::themes::find(&c.theme).map(|e| e.name.to_string()).unwrap_or_else(|| app.theme.name.clone())
                    )
                }
                "motion" => format!("{:?}{}", c.motion, if app.motion() != c.motion { " (reduced over SSH)" } else { "" }).to_lowercase(),
                "mouse" => {
                    let now = match app.pointer_mode() {
                        super::super::term::Pointer::Hover => "clicks, wheel and hover",
                        super::super::term::Pointer::Clicks => "clicks and wheel",
                        super::super::term::Pointer::Off => "off",
                    };
                    format!("{} · {now} · shift-drag selects text", format!("{:?}", c.mouse).to_lowercase())
                }
                "icons" => {
                    use super::super::icons::{Icon, Set};
                    let set = app.icon_set();
                    let mode = format!("{:?}", c.icons).to_lowercase();
                    match set {
                        // Ask, since no terminal can say which font it draws with.
                        Set::Nerd => format!(
                            "{mode} · nerd font · {} {} {}  a wallet, a lock and a rocket?",
                            Icon::Wallet.glyph(set),
                            Icon::Lock.glyph(set),
                            Icon::Launch.glyph(set)
                        ),
                        Set::Unicode => format!("{mode} · unicode"),
                        Set::Ascii => format!("{mode} · ascii"),
                    }
                }
                "background" => {
                    use wallet_core::config::BackgroundMode;
                    let now = if app.see_through().is_some() { "the terminal's, translucency kept" } else { "the theme's, solid" };
                    let mode = match c.background {
                        BackgroundMode::Auto => "auto",
                        BackgroundMode::Terminal => "terminal",
                        BackgroundMode::Solid => "solid",
                    };
                    format!("{mode} · {now}")
                }
                "daemon" => {
                    let running = match crate::daemon::state(&app.paths) {
                        Some(s) => format!(
                            "running · {}, {} unlocked",
                            amount::count(s.wallets.len(), "wallet"),
                            s.wallets.iter().filter(|w| w.2).count()
                        ),
                        None => "not running".into(),
                    };
                    if c.daemon_autostart {
                        format!("{on} starts with the terminal · {running}")
                    } else {
                        format!("{off} off · {running}")
                    }
                }
                "daemon_unlock" => {
                    if c.daemon_share_unlock {
                        format!("{on} each unlock here unlocks it in the daemon")
                    } else {
                        format!("{off} off")
                    }
                }
                "layout" => match c.layout.as_str() {
                    "trader" => "trader · Markets beside the swap card".into(),
                    "focus" => "focus · no sidebar".into(),
                    "standard" => "standard".into(),
                    _ => "auto · trader from 200 columns".into(),
                },
                "feature:messaging" => feature_value(t, c.features.messaging, "board, sealed DMs, chat dock"),
                "feature:trading" => feature_value(t, c.features.trading, "markets, swap, pools, launches"),
                "feature:nfts" => feature_value(t, c.features.nfts, "collected, explore, listings"),
                "ceremonies" => on_off(c.ceremonies),
                "sound" => on_off(c.sound),
                "hold_to_sign" => on_off(c.hold_to_sign),
                "vim_keys" => format!("{} · the arrows always move", on_off(c.vim_keys)),
                "big_numbers" => on_off(c.big_numbers),
                "balance_in_bar" => format!("{} · $ toggles it", on_off(c.balance_in_bar)),
                "lock_effect" => format!("{}  {}", c.lock_effect, t.icon(Icon::Disclosure)),
                "lock_loop" => {
                    if c.lock_loop {
                        format!("{} · one after another", on_off(true))
                    } else {
                        format!("{} · one per lock, then still", on_off(false))
                    }
                }
                "notifications" => on_off(c.notifications),
                "autolock" => {
                    if c.auto_lock_minutes == 0 {
                        format!("{off} off")
                    } else {
                        format!("{} min", c.auto_lock_minutes)
                    }
                }
                "prices" => on_off(c.fetch_prices),
                "ipfs" | "abi_ipfs" => {
                    let content = if *id == "abi_ipfs" { wallet_core::ipfs::Content::Abi } else { wallet_core::ipfs::Content::Media };
                    let g = wallet_core::ipfs::gateway(content);
                    let kind = if g.is_local() {
                        "your node"
                    } else if g.is_default_for(content) {
                        "default"
                    } else {
                        "custom"
                    };
                    format!("{} · {kind}  ›", g.display())
                }
                _ => t.icon(Icon::Disclosure).into(),
            };
            let row = Row::new(vec![Cell::from(format!("  {label}")), Cell::from(super::super::views::value_line(t, value))]);
            (*id, if i == app.nav.selected { row.style(t.selected()) } else { row })
        })
        .collect();
    // Grouped: a heading wherever the group changes. Display lines are headings and rows; the
    // cursor moves over rows only.
    let mut lines: Vec<(Option<usize>, Row)> = Vec::new();
    let mut group = "";
    for (i, (id, row)) in rows.into_iter().enumerate() {
        let g = app::setting_group(id);
        if g != group {
            if !group.is_empty() {
                lines.push((None, Row::new(vec![Cell::from("")])));
            }
            lines.push((None, Row::new(vec![Cell::from(Span::styled(g.to_string(), t.strong_style()))])));
            group = g;
        }
        lines.push((Some(i), row));
    }
    let [list, info] = Layout::vertical([Constraint::Length(lines.len() as u16 + 2), Constraint::Min(4)]).areas(area);
    let block = panel(t, "settings", true);
    let inner = block.inner(list);
    f.render_widget(block, list);
    // A short terminal cannot show every line: the list scrolls with the cursor, and says when
    // there is more below.
    let n = lines.len();
    let at = lines.iter().position(|(i, _)| *i == Some(app.nav.selected)).unwrap_or(0);
    let mut visible = (inner.height as usize).max(1);
    let more_below = |offset: usize, visible: usize| n.saturating_sub(offset + visible);
    let mut offset = app.list_window(app.main_list(), at, n, visible);
    if more_below(offset, visible) > 0 && visible > 1 {
        visible -= 1;
        offset = app.list_window(app.main_list(), at, n, visible);
    }
    {
        let mut hits = app.input.hits.borrow_mut();
        for (k, (i, _)) in lines.iter().enumerate().skip(offset).take(visible) {
            if let Some(i) = i {
                let rect = Rect::new(inner.x, inner.y + (k - offset) as u16, inner.width, 1);
                hits.add(rect, Target::Row { list: app.main_list(), index: *i, key: settings.get(*i).map(|s| s.0.to_string()) });
            }
        }
    }
    let below = more_below(offset, visible);
    let mut shown: Vec<Row> = lines.into_iter().skip(offset).take(visible).map(|(_, r)| r).collect();
    if below > 0 {
        shown.push(Row::new(vec![Cell::from(Span::styled(format!("  ↓ {below} more"), t.dim_style()))]));
    }
    f.render_widget(Table::new(shown, [Constraint::Length(32), Constraint::Min(20)]), inner);
    let caps = &app.term.caps;
    let lines = vec![
        Line::from(vec![
            Span::styled("terminal      ", t.dim_style()),
            Span::raw({
                let tier = match caps.tier {
                    Tier::Pixels => "pixel QR codes",
                    Tier::Cells => "block QR codes",
                    Tier::Text => "no QR codes",
                };
                let mut parts =
                    vec![caps.terminal.clone(), tier.to_string(), if caps.truecolor { "24-bit color".into() } else { "256 colors".into() }];
                if caps.tmux {
                    parts.push("inside tmux".into());
                }
                if caps.ssh {
                    parts.push("over SSH (reduced motion)".into());
                }
                parts.join(" · ")
            }),
        ]),
        Line::from(vec![
            Span::styled("theme         ", t.dim_style()),
            Span::raw(format!("{} · {}", app.theme.name, app::short_path(&app.theme.source))),
            Span::styled(app.theme.text_contrast().map(|c| format!("  · text contrast {c:.1}:1")).unwrap_or_default(), t.dim_style()),
        ]),
        Line::from(vec![
            Span::styled("data          ", t.dim_style()),
            Span::raw(app::short_path(&app.paths.root().display().to_string())),
        ]),
        Line::from(""),
        Line::from(Span::styled("With theme = auto the wallet follows Omarchy live; T opens the showroom from anywhere.", t.dim_style())),
    ];
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).block(panel(t, "environment", false)), info);
}
