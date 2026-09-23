//! Everything drawn over a screen: reviews, forms, the palette, the sheet, help, confirmations.

use super::*;

// ---------------------------------------------------------------- modals

/// Token icons or the NFT thumbnail for a review, with their names as text beside them.
pub(crate) fn draw_review_pictures(f: &mut Frame, app: &App, t: &Theme, area: Rect, visuals: &[wallet_core::tx::ReviewVisual]) {
    // Only the review's own pictures are placed while it is open.
    app.eco.kitty.borrow_mut().clear();
    let bg = Style::default().bg(t.raised);
    if let Some(v) = visuals.iter().find(|v| v.role == "nft") {
        let id = v.token_id.clone().unwrap_or_default();
        let pic = Rect { width: 16.min(area.width), ..area };
        super::super::images::picture(
            app,
            f.buffer_mut(),
            pic,
            t,
            app.nft_image_url(&v.contract, &id).as_deref(),
            &v.symbol,
            &v.contract,
            true,
        );
        let text = Rect { x: area.x + pic.width + 2, width: area.width.saturating_sub(pic.width + 2), ..area };
        let lines = vec![
            Line::from(Span::styled(v.symbol.clone(), t.strong_style())),
            Line::from(Span::styled(format!("token #{id}"), t.dim_style())),
            Line::from(Span::styled(short_address(&v.contract), Style::default().fg(t.link))),
        ];
        f.render_widget(Paragraph::new(lines).style(bg), text);
        return;
    }
    let mut x = area.x;
    for (i, v) in visuals.iter().enumerate() {
        if i > 0 {
            f.render_widget(Paragraph::new(Line::from(Span::styled(" → ", t.dim_style()))).style(bg), Rect::new(x, area.y + 1, 3, 1));
            x += 4;
        }
        let pic = Rect::new(x, area.y, 4, 2);
        if pic.right() > area.right() {
            break;
        }
        super::super::images::picture(
            app,
            f.buffer_mut(),
            pic,
            t,
            app.asset_icon_url(&v.contract).as_deref(),
            &v.symbol,
            &v.contract,
            false,
        );
        let label = match v.role.as_str() {
            "pay" => "you pay",
            "receive" => "you receive",
            _ => "token",
        };
        let w = (v.symbol.chars().count().max(label.len()) as u16 + 1).min(area.right().saturating_sub(x + 5));
        let lines = vec![Line::from(Span::styled(label, t.dim_style())), Line::from(Span::styled(v.symbol.clone(), t.strong_style()))];
        f.render_widget(Paragraph::new(lines).style(bg), Rect::new(x + 5, area.y, w, 2));
        x += 5 + w + 1;
    }
}

/// Dim everything already drawn, so the modal about to be drawn over it is the one lit thing.
/// Text and fills fall toward the page color; the page itself is untouched, so a see-through
/// background stays see-through. A lit border behind stops being a focus color, so the edge
/// painter no longer finds (and lights) it. The bottom row stays lit: the footer names the
/// modal's own keys.
pub(crate) fn scrim(buf: &mut ratatui::buffer::Buffer, t: &Theme, badges: &[(String, Color)]) {
    // The header and the footer are chrome, not the page: they stay lit (the footer names the
    // modal's keys, and the header keeps the wallet's logo and state in view).
    let area = Rect { y: buf.area.y + 1, height: buf.area.height.saturating_sub(2), ..buf.area };
    let toward = match t.surface {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    };
    for y in area.y..area.bottom() {
        // A token badge behind the glass keeps its color and loses its letters: its bitmap is
        // taken down under a modal, and two letters on a dimmed tile read as a stray word.
        for x in area.x..area.right().saturating_sub(1) {
            let (a, b) = (&buf[(x, y)], &buf[(x + 1, y)]);
            let letters = format!("{}{}", a.symbol(), b.symbol());
            if a.modifier.contains(Modifier::BOLD) && badges.iter().any(|(l, bg)| *l == letters && a.bg == *bg && b.bg == *bg) {
                buf[(x, y)].set_symbol(" ");
                buf[(x + 1, y)].set_symbol(" ");
            }
        }
        for x in area.x..area.right() {
            let Some(cell) = buf.cell_mut((x, y)) else { continue };
            match (toward, cell.fg) {
                (Some(s), Color::Rgb(r, g, b)) => cell.fg = blend((r, g, b), s, 0.62),
                // An ANSI palette can't be mixed: faint is the terminal's own dimming.
                _ => cell.modifier.insert(Modifier::DIM),
            }
            if let (Some(s), Color::Rgb(r, g, b)) = (toward, cell.bg)
                && cell.bg != t.surface
            {
                cell.bg = blend((r, g, b), s, 0.6);
            }
            // Bold behind a modal still shouts.
            cell.modifier.remove(Modifier::BOLD);
        }
    }
}

pub(crate) fn blend(a: (u8, u8, u8), b: (u8, u8, u8), amount: f32) -> Color {
    let m = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * amount).round() as u8;
    Color::Rgb(m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

pub(crate) fn draw_modal(f: &mut Frame, app: &mut App, t: &Theme, area: Rect) {
    // An open modal has the pointer: nothing behind it answers a click (see `hit`).
    if !matches!(app.modal, Modal::None) {
        app.hits.borrow_mut().capture(area);
        let badges: Vec<(String, Color)> = app.eco.inline_icons.borrow().iter().map(|(l, c, _)| (l.clone(), *c)).collect();
        scrim(f.buffer_mut(), t, &badges);
    }
    let palette = match &app.modal {
        Modal::Palette { query, .. } => app.palette_entries(query),
        _ => Vec::new(),
    };
    // What the action sheet acts on, named the way the header names it.
    let here = app.breadcrumb().last().cloned().unwrap_or_default().to_lowercase();
    let dash = &app.dash;
    match &mut app.modal {
        Modal::None => {}
        Modal::Form(form) => {
            // A limit order's note is its live preview: what the typed target means now.
            if let super::super::app::FormKind::OrderCreate { preview, slippage, .. } = &form.kind {
                form.note = Some(super::super::order_ui::note(preview, *slippage, &form.fields));
            }
            // Room for the notes as they wrap at the modal's width.
            let wrapped = |n: &Option<String>| {
                n.as_ref().map_or(0, |n| n.lines().map(|l| (l.chars().count() as u16).div_ceil(80).max(1)).sum::<u16>() + 1)
            };
            let h = form.fields.len() as u16 * 3 + 5 + wrapped(&form.note) + wrapped(&form.contract_note);
            let rect = centered(area, 84, h);
            let title = form.title.clone();
            let inner = modal_frame(f, rect, t, &title);
            let mut lines = Vec::new();
            // What the destination turned out to be goes first: it can change what this form is
            // even for, so it is read before the amount is typed.
            if let Some(n) = &form.contract_note {
                lines.push(Line::from(Span::styled(format!("{} {n}", t.icon(Icon::Attention)), Style::default().fg(t.attention))));
                lines.push(Line::from(""));
            }
            if let Some(n) = &form.note {
                // Its first paragraph says what the form is for; any after it (a limit order's
                // live preview) are ordinary text, one line each.
                let mut rest = false;
                for line in n.lines() {
                    rest |= line.is_empty();
                    let style = if rest { t.text_style() } else { Style::default().fg(t.attention) };
                    lines.push(Line::from(Span::styled(line.to_string(), style)));
                }
                lines.push(Line::from(""));
            }
            let account_value = form
                .fields
                .iter()
                .find(|fl| matches!(fl.kind, FieldKind::Choice(_)) && fl.value.starts_with("0x"))
                .map(|fl| fl.value.clone());
            let available = |asset: &str| -> Option<String> {
                match asset {
                    "QUAI" => {
                        let a = account_value
                            .as_ref()
                            .and_then(|v| dash.accounts.iter().find(|a| a.address == *v))
                            .or(dash.accounts.first())?;
                        Some(format!("{} QUAI", q(a.balance)))
                    }
                    "QI" => dash.qi.as_ref().map(|s| format!("{} Qi", qi(s.balance.spendable))),
                    "WQI" => dash.wrap.as_ref().and_then(|w| w.wqi_qi.clone()).map(|v| format!("{v} WQI")),
                    "WQUAI" => dash
                        .wrap
                        .as_ref()
                        .and_then(|w| w.wquai_atoms.clone())
                        .map(|v| format!("{} WQUAI", q(v.parse().unwrap_or_default()))),
                    _ => None,
                }
            };
            let error = form.error.as_deref().map(|e| (form.error_field.unwrap_or(usize::MAX), e));
            let first_field = lines.len();
            let focused = push_fields(&mut lines, t, &form.fields, form.focus, error, Some(&available), inner.width);
            if let (Some(msg), None) = (&form.error, form.error_field) {
                lines.push(Line::from(Span::styled(format!("{} {msg}", t.icon(Icon::Danger)), Style::default().fg(t.danger))));
            }
            lines.push(Line::from(if form.pending {
                Span::styled(
                    format!("{} preparing… nothing is signed until you approve the review", spinner()),
                    Style::default().fg(t.pending),
                )
            } else {
                Span::styled("tab/↑↓ fields · ←/→ choices · enter continue · esc cancel", t.dim_style())
            }));
            // A contract call can declare any number of arguments, so this is the first form whose
            // height is not known in advance. Rather than draw the tail of it off the bottom of
            // the screen — where the cursor would still move into fields nobody can see — the
            // content scrolls to keep the focused field in view.
            //
            // The offset is derived from the focus every frame rather than stored: there is no
            // second piece of state to fall out of step with the cursor.
            //
            // The footer keeps a row of its own below the scrolling part, because a form cut off
            // at the bottom must never read as a complete one — the line that says so has to stay
            // on screen, which it would not if it scrolled with everything else.
            let footer = lines.pop().unwrap_or_else(|| Line::from(""));
            let body = Rect { height: inner.height.saturating_sub(1), ..inner };
            let viewport = body.height as usize;
            let fields_start = first_field;
            let (scroll, above, below) = if lines.len() > viewport {
                let last = lines.len().saturating_sub(viewport);
                // Far enough down to show the whole focused field, and no further than the end.
                let scroll = focused.end.saturating_sub(viewport).min(last).min(focused.start);
                (scroll, scroll, lines.len().saturating_sub(scroll + viewport))
            } else {
                (0, 0, 0)
            };
            f.render_widget(
                Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((scroll as u16, 0)).style(Style::default().bg(t.raised)),
                body,
            );
            {
                // Each field is three lines (label and value, the track, a note): a click on the
                // first two focuses it; on a choice's value, it moves to the next option.
                let mut hits = app.hits.borrow_mut();
                hits.add(rect, Target::Swallow);
                hits.add(body, Target::Scroll(super::super::hit::Scroll::Form));
                let label_w = form.fields.iter().map(|fl| fl.label.chars().count()).max().unwrap_or(10).max(10) + 2;
                for (i, field) in form.fields.iter().enumerate() {
                    let line = fields_start + 3 * i;
                    if line < scroll || line >= scroll + viewport {
                        continue;
                    }
                    let y = body.y + (line - scroll) as u16;
                    let rows = if line + 1 < scroll + viewport { 2 } else { 1 };
                    hits.add(Rect::new(body.x, y, body.width, rows), Target::Field(i));
                    if let FieldKind::Choice(options) = &field.kind
                        && !options.is_empty()
                    {
                        let current = options.iter().position(|(v, _)| *v == field.value).unwrap_or(0);
                        let x = body.x + 1 + label_w as u16;
                        let w = body.right().saturating_sub(x);
                        hits.add(Rect::new(x, y, w, 1), Target::Choice { field: i, option: (current + 1) % options.len() });
                    }
                }
            }
            let mut marks = Vec::new();
            if above > 0 {
                marks.push(format!("↑ {above} more"));
            }
            if below > 0 {
                marks.push(format!("↓ {below} more"));
            }
            let footer = if marks.is_empty() {
                footer
            } else {
                Line::from(vec![
                    Span::styled(format!("{}  ", marks.join(" · ")), Style::default().fg(t.attention)),
                    Span::styled("tab/↑↓ fields · enter continue · esc cancel", t.dim_style()),
                ])
            };
            let footer_row = Rect { y: inner.y + body.height, height: 1, ..inner };
            f.render_widget(Paragraph::new(footer).style(Style::default().bg(t.raised)), footer_row);
        }
        Modal::Review(r) => {
            let rv = &r.review;
            let mut lines: Vec<Line> = Vec::new();
            // A step of a sequence says where it stands in it, first.
            if let Some(flow) = app.eco.flow.as_ref().filter(|f| f.review_op.as_deref() == Some(rv.op_id.as_str())) {
                let steps = flow.stepper(Some(&super::super::eco::step_name(&rv.kind)));
                let at = steps.iter().position(|(_, s)| *s == super::super::eco::StepState::Now).map_or(0, |i| i + 1);
                let mut line = vec![Span::styled(format!("step {at} of {} · ", steps.len()), t.dim_style())];
                line.extend(super::super::widgets::stepper(t, &steps));
                lines.push(Line::from(line));
                lines.push(Line::from(""));
            }
            for w in &rv.warnings {
                // A first send is a moment to check, not an alarm; everything else is.
                let line = if w.starts_with("first time sending") {
                    Span::styled(format!("{} {w}", t.icon(Icon::Attention)), Style::default().fg(t.attention))
                } else if w.starts_with("possible address poisoning") || w.contains("only ever sent you dust") {
                    // A filled pill, not reversed text: reverse video over a see-through page
                    // takes whatever colour is behind the window.
                    super::super::widgets::pill(t, &format!("{} {w}", t.icon(Icon::Warning)), t.danger)
                } else {
                    Span::styled(format!("{} {w}", t.icon(Icon::Warning)), Style::default().fg(t.danger).add_modifier(Modifier::BOLD))
                };
                lines.push(Line::from(line));
            }
            if !rv.warnings.is_empty() {
                lines.push(Line::from(""));
            }
            // The outcome first, in one glance: what leaves, what arrives, what the fee can be.
            if !rv.changes.is_empty() {
                lines.push(Line::from(Span::styled("balance changes", t.strong_style())));
                let amount_w = rv.changes.iter().map(|c| c.amount.chars().count()).max().unwrap_or(0);
                let asset_w = rv.changes.iter().map(|c| c.asset.chars().count()).max().unwrap_or(0).min(24);
                for c in &rv.changes {
                    let (sign, color) = match c.direction.as_str() {
                        "out" => ("−", t.danger),
                        "in" => ("+", t.ok),
                        "fee" => ("−", t.attention),
                        _ => ("·", t.dim),
                    };
                    let mut spans = vec![Span::styled(format!("  {sign} "), Style::default().fg(color).add_modifier(Modifier::BOLD))];
                    if c.direction != "none" {
                        spans.push(Span::styled(format!("{:>amount_w$} ", c.amount), t.strong_style()));
                        spans.push(Span::styled(format!("{:<asset_w$}   ", truncate(&c.asset, asset_w)), Style::default().fg(color)));
                    }
                    spans.push(Span::styled(c.note.clone(), t.dim_style()));
                    lines.push(Line::from(spans));
                }
                lines.push(Line::from(""));
            }
            let label_w = rv.fields.iter().map(|fl| fl.label.chars().count() + 2).max().unwrap_or(0).clamp(16, 44);
            let kv = |k: &str, v: String, style: Style| {
                Line::from(vec![Span::styled(format!("{k:<label_w$}"), t.dim_style()), Span::styled(v, style)])
            };
            let asset_color = if rv.asset.eq_ignore_ascii_case("QI") { t.qi } else { t.quai };
            lines.push(kv("network", rv.network.clone(), Style::default().fg(t.link)));
            // Addresses in groups of four, to be checked group by group against another copy.
            let addr = |k: &str, v: &str| {
                let mut spans = vec![Span::styled(format!("{k:<label_w$}"), t.dim_style())];
                spans.extend(super::super::widgets::address(t, v, t.text_style()));
                Line::from(spans)
            };
            lines.push(addr("from", &rv.from));
            lines.push(addr("to", &rv.to));
            let amount_text = if rv.amount.contains(' ') { rv.amount.clone() } else { format!("{} {}", rv.amount, rv.asset) };
            // The figure the review settles, underlined twice where the terminal can.
            lines.push(kv(
                "amount",
                amount_text,
                t.strong_style().fg(asset_color).add_modifier(super::super::term::backend::underline::DOUBLE).underline_color(asset_color),
            ));
            // A fee above the fee policy is highlighted, never blocked: approving sends it.
            let fee_text =
                format!("{}{}", rv.max_fee, rv.fee_bps.map(|b| format!("  ({}.{:02}% of amount)", b / 100, b % 100)).unwrap_or_default());
            if rv.fee_over_policy {
                lines.push(Line::from(vec![
                    Span::styled(format!("{:<label_w$}", "max fee"), t.dim_style()),
                    Span::styled(
                        fee_text,
                        t.strong_style().fg(t.danger).add_modifier(super::super::term::backend::underline::CURLY).underline_color(t.danger),
                    ),
                    Span::raw("  "),
                    super::super::widgets::pill(t, &format!("{} above fee policy", t.icon(Icon::Up)), t.danger),
                ]));
            } else {
                lines.push(kv("max fee", fee_text, Style::default().fg(t.attention)));
            }
            for field in &rv.fields {
                let mut label = field.label.clone();
                if let Some(first) = label.get(..1) {
                    label = format!("{}{}", first.to_lowercase(), &label[1..]);
                }
                lines.push(kv(&label, field.value.clone(), t.text_style()));
            }
            // Plain-language outcome, and the worst-case balance afterwards when it's knowable.
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("what happens", t.strong_style())));
            for step in review_story(&rv.kind) {
                lines.push(Line::from(vec![Span::styled("  → ", t.dim_style()), Span::styled(step, t.text_style())]));
            }
            // Said once, plainly, on every review: a signed transaction has no undo.
            lines.push(Line::from(vec![
                Span::styled("  ! ", t.strong_style().fg(t.attention)),
                Span::styled("once signed and broadcast, it can't be undone", t.strong_style()),
            ]));
            if rv.asset == "QUAI"
                && let Some(acct) = dash.accounts.iter().find(|a| rv.from.starts_with(&a.address))
                && let (Ok(amount_base), Some(fee_text)) = (rv.amount_base.parse::<U256>(), rv.max_fee.split_whitespace().next())
                && let Ok(fee) = amount::parse_quai(fee_text)
            {
                let spent = amount_base.saturating_add(fee);
                let after = acct.balance.saturating_sub(spent);
                lines.push(Line::from(vec![
                    Span::styled("  → ", t.dim_style()),
                    Span::styled(format!("{} keeps at least {} QUAI", acct.label, q(after)), t.text_style()),
                ]));
            }
            if !rv.coins.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled("Qi inputs and outputs", t.strong_style())));
                for c in &rv.coins {
                    let (glyph, color) = match c.role.as_str() {
                        "input" => ("−", t.dim),
                        "change" => ("↩", t.ok),
                        _ => ("→", t.qi),
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {glyph} {:<10}", c.role), Style::default().fg(color)),
                        Span::raw(format!("{:>16} Qi  ", super::super::num::qi(U256::from(c.qits)))),
                        Span::styled(c.address.clone(), t.dim_style()),
                    ]));
                }
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("— end of review —", t.dim_style())).alignment(Alignment::Center));
            // Fit the modal to the content (wrapped at the modal's inner width) up to the screen.
            let est_width = 100u16.min(area.width.saturating_sub(2)).saturating_sub(6).max(1) as usize;
            let est_lines: usize = lines.iter().map(|l| l.width().max(1).div_ceil(est_width)).sum();
            // Pictures of the assets involved sit above the text (tokens: a 2-row strip; NFT: 8 rows).
            let visuals = rv.visuals.clone();
            let strip_h: u16 = if visuals.iter().any(|v| v.role == "nft") {
                8
            } else if visuals.is_empty() {
                0
            } else {
                3
            };
            let rect = centered(area, 100, (est_lines as u16 + 6 + strip_h).min(area.height.saturating_sub(4)));
            let title = format!("review · {}", rv.title);
            let inner = modal_frame(f, rect, t, &title);
            let [mut body, buttons] = Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).areas(inner);
            let mut strip = None;
            if strip_h > 0 && body.height >= strip_h + 8 && !app.plain {
                let [s, rest] = Layout::vertical([Constraint::Length(strip_h), Constraint::Min(3)]).areas(body);
                strip = Some(s);
                body = rest;
            }
            let width = body.width.max(1) as usize;
            r.content_lines = lines.iter().map(|l| l.width().max(1).div_ceil(width)).sum::<usize>() as u16;
            r.viewport = body.height;
            r.scroll = r.scroll.min(r.content_lines.saturating_sub(r.viewport));
            f.render_widget(
                Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((r.scroll, 0)).style(Style::default().bg(t.raised)),
                body,
            );
            use super::super::widgets::{ButtonState, button};
            let can = r.can_approve();
            let reject = button(
                t,
                "Reject",
                if r.approve_focused { "tab" } else { "enter" },
                t.text,
                if r.approve_focused { ButtonState::Ready } else { ButtonState::Focused },
            );
            // Not yet signable: the button says what it waits for.
            let approve = match (can, r.approve_focused) {
                (false, _) => button(t, "Approve & sign", "read to enable", t.ok, ButtonState::Waiting),
                (true, true) if app.config.hold_to_sign => button(t, "Approve & sign", "hold enter", t.ok, ButtonState::Focused),
                (true, true) => button(t, "Approve & sign", "enter", t.ok, ButtonState::Focused),
                (true, false) => button(t, "Approve & sign", "tab", t.ok, ButtonState::Ready),
            };
            // (Read from the field: the modal is borrowed.)
            let held = app
                .hold
                .as_ref()
                .filter(|(id, _, last)| *id == rv.op_id && last.elapsed() < app::HOLD_GAP && can && r.approve_focused)
                .map(|(_, start, _)| (start.elapsed().as_secs_f64() / app::HOLD_TO_SIGN.as_secs_f64()).min(1.0));
            let mut spans = vec![
                reject,
                Span::raw("   "),
                approve,
                Span::raw("   "),
                Span::styled(if held.is_some() { "hold " } else { "read " }, t.dim_style()),
            ];
            match held {
                // The bar fills while Enter is held; it signs when it is full.
                Some(p) => spans.extend(super::super::widgets::meter(t, p, 14, t.ok)),
                None => spans.extend(super::super::widgets::meter(t, r.read_ratio(), 14, if can { t.ok } else { t.pending })),
            }
            spans.push(Span::styled(if can { "  esc rejects" } else { "  space/j to read on · esc rejects" }, t.dim_style()));
            let mut line = Line::from(spans);
            // Narrow: the hint shortens, then goes; then the meter gives way to a percentage. The
            // two buttons (spans 0 and 2, which the pointer targets) always stay whole.
            let fits = |l: &Line| l.width() <= buttons.width as usize;
            if !fits(&line) {
                *line.spans.last_mut().expect("hint") = Span::styled(if can { "  esc" } else { "  space reads" }, t.dim_style());
            }
            if !fits(&line) {
                line.spans.pop();
            }
            if !fits(&line) {
                line.spans.truncate(4);
                line.spans.push(Span::styled(format!("{:.0}% read", r.read_ratio() * 100.0), t.dim_style()));
            }
            if !fits(&line) {
                line.spans.truncate(3);
            }
            {
                // Reject rejects; Approve only arms (the key signs). The body scrolls with the
                // wheel, which counts as reading the same way the arrow keys do.
                let mut hits = app.hits.borrow_mut();
                hits.add(rect, Target::Swallow);
                hits.add(body, Target::Scroll(super::super::hit::Scroll::Review));
                let row = Rect { y: buttons.y + 1, height: 1, ..buttons };
                hits.spans(row, &line.spans, |i| match i {
                    0 => Some(Target::Review(super::super::hit::ReviewPart::Reject)),
                    2 => Some(Target::Review(super::super::hit::ReviewPart::Approve)),
                    _ => None,
                });
            }
            f.render_widget(Paragraph::new(vec![Line::from(""), line]).style(Style::default().bg(t.raised)), buttons);
            if let Some(strip) = strip {
                draw_review_pictures(f, app, t, strip, &visuals);
            }
        }
        Modal::Help => {
            use super::super::keymap::{self as km, Group, Verb};
            let keys = app.keys_here();
            let moved = app.help_moved;
            let rect = centered(area, 100, area.height.saturating_sub(2));
            let inner = modal_frame(f, rect, t, "keys");
            // Wide enough for the longest binding ("backspace ctrl-o") and a space after it.
            let key_w = 18usize;
            let row = |k: String, v: &str| {
                Line::from(vec![
                    Span::styled(format!("  {k:<key_w$}"), t.strong_style().fg(t.focus)),
                    Span::styled(v.to_string(), t.text_style()),
                ])
            };
            let mut lines = Vec::new();
            if moved {
                lines.push(Line::from(Span::styled("What moved", t.strong_style().fg(t.attention))));
                for l in [
                    "  Sections: 1 Home · 2 Markets (pairs, launches, pools) · 3 Trade · 4 NFTs · 5 People · 6 Activity.",
                    "  Trade › Exchange is one card for any pair: it swaps, converts QUAI ↔ Qi or wraps, as the pair needs.",
                    "  Limit orders have their own tab, Trade › Orders (g o).",
                    "  Time locks sit under Home › Accounts; a wide terminal shows the selected row beside its list.",
                    "  Backspace (or ctrl-o) goes back to where you were; every screen keeps its place.",
                    "  W switches wallet from anywhere, and so does a click on the wallet's name.",
                    "  One meaning per key, everywhere. ctrl-l locks (l moves right, like h j k).",
                    "  space opens everything the focused thing can do; : still finds anything.",
                    "  g then a letter goes straight to a screen: g m markets, g a activity, g s settings…",
                    "  The mouse works: click, double-click, the wheel. shift-drag still selects text.",
                ] {
                    lines.push(Line::from(Span::styled(l, t.text_style())));
                }
                lines.push(Line::from(""));
            }
            // This view: the verbs it gives a meaning, its footer's first.
            lines.push(Line::from(Span::styled(format!("on {}", app.breadcrumb().join(" › ")), t.strong_style())));
            let mut seen: Vec<Verb> = Vec::new();
            for v in keys.footer.iter().copied().chain(keys.overrides.iter().map(|o| o.verb)) {
                if seen.contains(&v) || v == Verb::Sheet {
                    continue;
                }
                seen.push(v);
                lines.push(row(km::key_of(v), app.verb_label(v)));
            }
            if !keys.sheet.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled("space · actions here", t.strong_style())));
                for item in keys.sheet {
                    lines.push(row(format!("space {}", item.key), item.label));
                }
            }
            // Everywhere: every binding, grouped, two to a line where the width allows.
            let half = (inner.width as usize).saturating_sub(4) / 2;
            for group in [Group::Move, Group::Go, Group::App, Group::Money, Group::Item, Group::View] {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(group.title(), t.strong_style())));
                let entries: Vec<(String, &str)> = km::GLOBAL
                    .iter()
                    .filter(|b| b.group == group)
                    .map(|b| (b.keys.iter().map(|k| k.label()).collect::<Vec<_>>().join(" "), b.label))
                    .collect();
                let entries: Vec<(String, &str)> = if group == Group::Go {
                    let sections: Vec<String> = app.sections().iter().map(|s| s.key().to_string()).collect();
                    std::iter::once((sections.join(" "), "sections")).chain(entries).collect()
                } else {
                    entries
                };
                for pair in entries.chunks(if half >= 44 { 2 } else { 1 }) {
                    let mut spans = Vec::new();
                    for (k, v) in pair {
                        spans.push(Span::styled(format!("  {k:<key_w$}"), t.strong_style().fg(t.focus)));
                        spans.push(Span::styled(
                            format!("{:<w$}", truncate(v, half.saturating_sub(key_w + 3)), w = half.saturating_sub(key_w + 2)),
                            t.text_style(),
                        ));
                    }
                    lines.push(Line::from(spans));
                }
            }
            lines.push(Line::from(""));
            lines.push(row("review".into(), "read to the end · tab to Approve · enter signs · esc rejects · y copies it as a command"));
            let terms = super::super::glossary::for_screen(app.screen);
            if !terms.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled("words here", t.strong_style())));
                for term in terms.iter().take(6) {
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {:<key_w$}", term.word), t.strong_style().fg(t.qi)),
                        Span::raw(truncate(term.meaning, (inner.width as usize).saturating_sub(key_w + 3))),
                    ]));
                }
                lines.push(Line::from(Span::styled("  g all words", t.dim_style())));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Prefer plain text? Every action has a CLI command with --output json (see the palette).",
                t.dim_style(),
            )));
            let viewport = inner.height.saturating_sub(1);
            let overflow = (lines.len() as u16).saturating_sub(viewport);
            app.help_scroll = app.help_scroll.min(overflow);
            let scroll = app.help_scroll;
            let below = overflow - scroll;
            {
                let mut hits = app.hits.borrow_mut();
                hits.add(rect, Target::Button(super::super::hit::Button::Close));
                hits.add(inner, Target::Scroll(super::super::hit::Scroll::Help));
            }
            f.render_widget(
                Paragraph::new(lines).scroll((scroll, 0)).style(Style::default().bg(t.raised)),
                Rect { height: viewport, ..inner },
            );
            // The last row says whether there is more, and how to leave.
            let status = if below > 0 {
                Line::from(vec![
                    Span::styled(format!("↓ {below} more"), Style::default().fg(t.attention)),
                    Span::styled(" · j/k scroll · any other key closes", t.dim_style()),
                ])
            } else {
                Line::from(Span::styled("any key closes", t.dim_style()))
            };
            f.render_widget(
                Paragraph::new(status).style(Style::default().bg(t.raised)),
                Rect { y: inner.y + viewport, height: 1, ..inner },
            );
        }
        Modal::Palette { query, selected } => {
            let rect = centered(area, 110, 26);
            let inner = modal_frame(f, rect, t, "command palette");
            let entries = palette;
            let mut lines = vec![
                Line::from(vec![Span::styled("› ", Style::default().fg(t.focus)), Span::styled(format!("{query}▏"), t.strong_style())]),
                Line::from(Span::styled(
                    if query.is_empty() { "try: send alice 5 quai · swap 10 wqi to usdt · markets · theme" } else { "" },
                    t.dim_style(),
                )),
            ];
            // Rows below the query, less one for the selected entry's CLI line.
            let rows = inner.height.saturating_sub(4) as usize;
            let start =
                app.lists.borrow_mut().entry(super::super::hit::ListId::Palette).or_default().window(*selected, entries.len(), rows);
            {
                let mut hits = app.hits.borrow_mut();
                hits.add(rect, Target::Swallow);
                let list_area = Rect { y: inner.y + 2, height: rows as u16, ..inner };
                hits.rows(super::super::hit::ListId::Palette, list_area, start, entries.len(), |i| entries.get(i).map(|e| e.label.clone()));
            }
            let label_w = (inner.width as usize).saturating_sub(30).clamp(20, 48);
            let hint_w = (inner.width as usize).saturating_sub(label_w + 10).max(8);
            for (i, e) in entries.iter().enumerate().skip(start).take(rows) {
                let active = i == *selected;
                let style = if active { t.selected() } else { Style::default().bg(t.raised) };
                let tag_color = match e.tag {
                    "do" => t.ok,
                    "recent" => t.pending,
                    "contact" => t.link,
                    "asset" | "market" => t.quai,
                    "go" => t.text,
                    "term" => t.qi,
                    "chat" => t.link,
                    _ => t.dim,
                };
                lines.push(Line::from(vec![
                    Span::styled(if active { "▌" } else { " " }, Style::default().fg(t.focus)),
                    Span::styled(format!("{:<7}", e.tag), Style::default().fg(tag_color)),
                    Span::styled(format!("{:<label_w$}", truncate(&e.label, label_w)), style),
                    Span::styled(format!(" {}", truncate(&e.hint, hint_w)), t.dim_style()),
                ]));
            }
            if entries.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("Nothing matches among {} actions, screens, contacts and markets.", ACTIONS.len()),
                    t.dim_style(),
                )));
            }
            let [list, cli] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
            f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), list);
            // The same thing from a shell, or what a term means, for the selected entry.
            let footer = match entries.get(*selected) {
                Some(e) if e.tag == "term" => Line::from(Span::styled(format!("  {}", e.hint), t.text_style())),
                Some(e) if !e.cli.is_empty() => {
                    Line::from(vec![Span::styled("  $ ", t.dim_style()), Span::styled(e.cli.clone(), t.text_style())])
                }
                _ => Line::from(Span::styled("  enter run · ↑↓ choose · esc close", t.dim_style())),
            };
            f.render_widget(Paragraph::new(footer).style(Style::default().bg(t.raised)), cli);
        }
        Modal::Receive { asset_qi, account } => {
            let (asset_qi, account) = (*asset_qi, *account);
            draw_receive(f, app, t, area, asset_qi, account);
        }
        Modal::Secret { text, title } => {
            let rect = centered(area, 88, 16);
            let inner = modal_frame(f, rect, t, "recovery phrase");
            let words: Vec<&str> = text.split_whitespace().collect();
            let mut lines = vec![Line::from(Span::styled(title.clone(), Style::default().fg(t.danger))), Line::from("")];
            let rows = words.len().div_ceil(4);
            for r in 0..rows {
                let mut spans = Vec::new();
                for c in 0..4 {
                    if let Some(w) = words.get(c * rows + r) {
                        spans.push(Span::styled(format!("{:>4} ", c * rows + r + 1), t.dim_style()));
                        spans.push(Span::styled(format!("{w:<14}"), t.strong_style()));
                    }
                }
                lines.push(Line::from(spans));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("enter or esc hides the phrase and wipes it from memory", t.dim_style())));
            // A click hides it too; nothing on this screen is ever copied.
            app.hits.borrow_mut().add(rect, Target::Button(super::super::hit::Button::Close));
            f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), inner);
        }
        Modal::Quote(qt) => {
            // The saturated and held cases trade the scenario bars for wrapped prose, so the height
            // has to follow the content rather than the scenario count.
            let extra = qt.hold.as_ref().map_or(0, |h| textwrap(&h.note, 84).len() + 1) + usize::from(qt.discount_saturated) * 5;
            let rect = centered(area, 88, (24 + qt.notes.len() + extra) as u16);
            let inner = modal_frame(f, rect, t, "conversion quote");
            app.hits.borrow_mut().add(rect, Target::Swallow);
            let label_w = qt.scenarios.iter().map(|s| s.label.chars().count()).max().unwrap_or(20) + 2;
            let mut lines = vec![
                Line::from(Span::styled(qt.headline.clone(), t.strong_style())),
                Line::from(""),
                Line::from(vec![Span::styled("you send     ", t.dim_style()), Span::styled(qt.amount_display.clone(), t.strong_style())]),
                Line::from(vec![
                    Span::styled("you receive  ", t.dim_style()),
                    Span::styled(
                        qt.expected_display
                            .clone()
                            .map(|e| format!("about {e} expected"))
                            .or_else(|| qt.quoted_display.clone())
                            .unwrap_or_else(|| "unavailable".into()),
                        t.strong_style().fg(t.ok),
                    ),
                    Span::styled(
                        qt.quoted_display
                            .clone()
                            .filter(|_| qt.expected_display.is_some() && qt.implied_slippage_bps.is_some_and(|b| b > 0))
                            .map(|s| format!("   rate {s}"))
                            .unwrap_or_default(),
                        t.dim_style(),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("block flow   ", t.dim_style()),
                    Span::raw(
                        qt.flow_amount
                            .clone()
                            .map(|v| format!("{} QUAI", q(v.parse().unwrap_or_default())))
                            .unwrap_or_else(|| "not reported".into()),
                    ),
                ]),
            ];
            // What the discount costs right now, which is the number the send/receive pair implies
            // but never states. Colour tracks severity so the size of the loss is legible at a
            // glance rather than read off two amounts.
            if let Some(bps) = qt.implied_slippage_bps.filter(|b| *b > 0) {
                let color = if bps >= 5000 {
                    t.danger
                } else if bps >= 500 {
                    t.attention
                } else {
                    t.ok
                };
                lines.push(Line::from(vec![
                    Span::styled("discount now ", t.dim_style()),
                    Span::styled(format!("{} below the rate", wallet_core::ops::percent(bps)), Style::default().fg(color)),
                ]));
            }
            lines.push(Line::from(""));
            if let Some(h) = &qt.hold {
                for l in textwrap(&h.note, 84) {
                    lines.push(Line::from(Span::styled(l, Style::default().fg(t.danger))));
                }
                lines.push(Line::from(""));
            }
            if qt.discount_saturated {
                // Four bars all reading 90% say nothing. One sentence that names the way out does.
                for l in textwrap(
                    "The discount is at its floor: at this size the protocol pays one tenth of the rate, and no slippage setting changes that. It grows with size against the block's conversion flow, so converting a smaller amount at a time loses far less — and the market route (wrap, swap, unwrap) is usually several times better.",
                    84,
                ) {
                    lines.push(Line::from(Span::styled(l, Style::default().fg(t.danger))));
                }
                lines.push(Line::from(""));
            } else if !qt.scenarios.is_empty() {
                lines.push(Line::from(Span::styled("if others convert in the same block…", t.strong_style())));
                let max_bps = qt.scenarios.iter().map(|s| s.discount_bps).max().unwrap_or(1).max(qt.suggested_slippage_bps).max(1);
                for s in &qt.scenarios {
                    let over = s.discount_bps > qt.suggested_slippage_bps;
                    let color = if over {
                        t.danger
                    } else if s.discount_bps > 100 {
                        t.attention
                    } else {
                        t.ok
                    };
                    let bar = (usize::from(s.discount_bps) * 30).div_ceil(usize::from(max_bps)).max(1);
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {:<label_w$}", s.label), t.text_style()),
                        Span::styled(format!("{:>7} ", wallet_core::ops::percent(s.discount_bps)), Style::default().fg(color)),
                        Span::styled("▇".repeat(bar), Style::default().fg(color)),
                        Span::styled(if over { " refund risk" } else { "" }, Style::default().fg(t.danger)),
                    ]));
                }
                lines.push(Line::from(""));
            }
            lines.push(Line::from(vec![
                Span::styled("suggested slippage ", t.dim_style()),
                Span::styled(
                    format!("{} ({} bps)", wallet_core::ops::percent(qt.suggested_slippage_bps), qt.suggested_slippage_bps),
                    t.strong_style(),
                ),
            ]));
            if let Some(m) = &qt.minimum {
                lines.push(Line::from(vec![Span::styled("minimum            ", t.dim_style()), Span::raw(m.clone())]));
            }
            lines.push(Line::from(""));
            for n in &qt.notes {
                lines.push(Line::from(Span::styled(format!("· {n}"), t.dim_style())));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("c ", t.strong_style().fg(t.focus)),
                Span::styled("QUAI→Qi  ", t.dim_style()),
                Span::styled("C ", t.strong_style().fg(t.focus)),
                Span::styled("Qi→QUAI  ", t.dim_style()),
                Span::styled("esc ", t.strong_style().fg(t.focus)),
                Span::styled("close", t.dim_style()),
            ]));
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).style(Style::default().bg(t.raised)), inner);
        }
        Modal::Result(s) => {
            let summary = app.eco.flow_summary.as_ref().is_some_and(|(_, s)| s.len() > 1);
            let rect = centered(area, 96, if summary { 14 } else { 12 });
            let inner = modal_frame(f, rect, t, "submitted");
            app.hits.borrow_mut().add(rect, Target::Swallow);
            let style = status_style(t, s.status);
            let mut lines = vec![
                Line::from(vec![
                    Span::styled(format!("{} ", status_glyph(t, s.status)), style),
                    Span::styled(s.message.clone(), t.strong_style()),
                ]),
                Line::from(""),
            ];
            // The last step of a sequence: the whole of it, done.
            if let Some((label, steps)) = app.eco.flow_summary.as_ref().filter(|(_, s)| s.len() > 1) {
                let done: Vec<(String, super::super::eco::StepState)> =
                    steps.iter().map(|s| (s.clone(), super::super::eco::StepState::Done)).collect();
                let mut line = vec![Span::styled(format!("{label} · "), t.dim_style())];
                line.extend(super::super::widgets::stepper(t, &done));
                lines.push(Line::from(line));
                lines.push(Line::from(""));
            }
            lines.extend([
                Line::from(vec![Span::styled("tx         ", t.dim_style()), Span::raw(s.tx_hash.clone())]),
                Line::from(vec![Span::styled("operation  ", t.dim_style()), Span::raw(s.op_id.clone())]),
            ]);
            if let Some(e) = &s.explorer {
                lines.push(Line::from(vec![
                    Span::styled("explorer   ", t.dim_style()),
                    Span::styled(e.clone(), Style::default().fg(t.link).add_modifier(Modifier::UNDERLINED)),
                ]));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("Tracked in {}. You'll get a notification when it lands. enter/esc close", Screen::Activity.place()),
                t.dim_style(),
            )));
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).style(Style::default().bg(t.raised)), inner);
        }
        Modal::Glossary { selected } => {
            let terms = super::super::glossary::TERMS;
            let rect = centered(area, 100, 30);
            let inner = modal_frame(f, rect, t, "glossary");
            let [list, meaning] = Layout::horizontal([Constraint::Length(24), Constraint::Min(20)]).areas(inner);
            let rows = list.height as usize;
            let start = app.lists.borrow_mut().entry(super::super::hit::ListId::Glossary).or_default().window(*selected, terms.len(), rows);
            {
                let mut hits = app.hits.borrow_mut();
                hits.add(rect, Target::Swallow);
                hits.rows(super::super::hit::ListId::Glossary, list, start, terms.len(), |_| None);
            }
            let items: Vec<Line> = terms
                .iter()
                .enumerate()
                .skip(start)
                .take(rows)
                .map(|(i, term)| {
                    let style = if i == *selected { t.selected() } else { Style::default().bg(t.raised) };
                    Line::from(Span::styled(format!(" {:<22}", term.word), style))
                })
                .collect();
            f.render_widget(Paragraph::new(items).style(Style::default().bg(t.raised)), list);
            let term = &terms[(*selected).min(terms.len() - 1)];
            let mut body = vec![
                Line::from(Span::styled(term.word, t.strong_style().fg(t.qi))),
                Line::from(""),
                Line::from(Span::styled(term.meaning, t.text_style())),
                Line::from(""),
            ];
            if !term.screens.is_empty() {
                let wher: Vec<String> = term.screens.iter().map(|s| format!("{} › {}", s.section().title(), s.title())).collect();
                body.push(Line::from(vec![Span::styled("seen on  ", t.dim_style()), Span::raw(wher.join(" · "))]));
            }
            body.push(Line::from(""));
            body.push(Line::from(Span::styled("j/k move · any other key closes · : finds a word too", t.dim_style())));
            f.render_widget(Paragraph::new(body).wrap(Wrap { trim: true }).style(Style::default().bg(t.raised)), meaning);
        }
        Modal::Notifications => {
            let rect = centered(area, 96, 24);
            let inner = modal_frame(f, rect, t, "notifications");
            app.hits.borrow_mut().add(rect, Target::Swallow);
            let lines: Vec<Line> = if dash.notifications.is_empty() {
                vec![Line::from(Span::styled("Quiet chain, quiet mind.", t.dim_style()))]
            } else {
                dash.notifications
                    .iter()
                    .take(inner.height as usize)
                    .map(|n| {
                        let text = format!("{} {}", n.title, n.body).to_lowercase();
                        let (g, c) = match n.level.as_str() {
                            "error" => (t.icon(Icon::Danger), t.danger),
                            "warn" => ("!", t.attention),
                            _ if text.contains("failed") || text.contains("refund") => (t.icon(Icon::Danger), t.danger),
                            _ if text.contains("settling") || text.contains("locked") || text.contains("submitted") => {
                                (t.icon(Icon::InFlight), t.pending)
                            }
                            "success" => (t.icon(Icon::Ok), t.ok),
                            _ => (t.icon(Icon::On), t.link),
                        };
                        let body = truncate(&n.body, (inner.width as usize).saturating_sub(n.title.chars().count() + 14));
                        Line::from(vec![
                            Span::styled(format!("{g} "), Style::default().fg(c)),
                            Span::styled(format!("{:>7}  ", ago(n.at)), t.dim_style()),
                            Span::styled(n.title.clone(), if n.read { t.text_style() } else { t.strong_style() }),
                            Span::styled(format!("  {body}"), t.dim_style()),
                        ])
                    })
                    .collect()
            };
            let mut lines = lines;
            if !app.log.is_empty() {
                lines.truncate((inner.height as usize).saturating_sub(app.log.len().min(6) + 2));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled("recent messages", t.strong_style())));
                for m in app.log.iter().take(6) {
                    let (g, c) = severity_mark(t, m.level);
                    lines.push(Line::from(vec![
                        Span::styled(format!("{g} "), Style::default().fg(c)),
                        Span::styled(truncate(&m.text, inner.width as usize - 2), t.text_style()),
                    ]));
                }
            }
            lines.truncate((inner.height as usize).saturating_sub(2));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Confirmations, receipts and alerts collect here · opening marks them read · 5 Activity has the full history · esc close",
                t.dim_style(),
            )));
            f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), inner);
        }
        Modal::Confirm { title, body, .. } => {
            let width = 64u16.min(area.width.saturating_sub(4));
            let wrapped = textwrap(body, width.saturating_sub(4) as usize);
            let rect = centered(area, width, wrapped.len() as u16 + 5);
            let title = title.clone();
            let inner = modal_frame(f, rect, t, &title);
            let mut lines: Vec<Line> = wrapped.into_iter().map(Line::from).collect();
            lines.push(Line::from(""));
            // Enter means no here, so "no" is the highlighted button, and it says so.
            let buttons = Line::from(vec![
                Span::styled(" n  no · enter ", t.selected()),
                Span::raw("   "),
                Span::styled(" y ", t.strong_style()),
                Span::styled(" yes", t.text_style()),
            ]);
            {
                let mut hits = app.hits.borrow_mut();
                hits.add(rect, Target::Swallow);
                let row = Rect { y: inner.y + lines.len() as u16, height: 1, ..inner };
                hits.spans(row, &buttons.spans, |i| match i {
                    0 => Some(Target::Confirm(false)),
                    2 | 3 => Some(Target::Confirm(true)),
                    _ => None,
                });
            }
            lines.push(buttons);
            f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), inner);
        }
        Modal::Notice { title, body, error, detail } => {
            let width = 72u16.min(area.width.saturating_sub(4));
            let mut lines: Vec<Line> = Vec::new();
            for (i, para) in body.iter().enumerate() {
                if para.is_empty() {
                    lines.push(Line::from(""));
                    continue;
                }
                for l in textwrap(para, width.saturating_sub(4) as usize) {
                    // The first sentence is the answer; it leads, in the state's color.
                    let style = if i == 0 { t.strong_style().fg(if *error { t.danger } else { t.ok }) } else { t.text_style() };
                    lines.push(Line::from(Span::styled(l, style)));
                }
            }
            if let Some(d) = detail {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled("operation  ", t.dim_style()), Span::styled(d.clone(), t.text_style())]));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("enter/esc close", t.dim_style())));
            let rect = centered(area, width, lines.len() as u16 + 2);
            let inner = modal_frame(f, rect, t, title);
            app.hits.borrow_mut().add(rect, Target::Swallow);
            f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), inner);
        }
        Modal::Sheet { items, selected } => {
            // Which-key style: small, at the bottom right above the footer, where the eye already
            // is after reading the hints; it never covers the list it acts on more than it must.
            let rows = items.len() as u16;
            let width = items.iter().map(|i| i.label.chars().count() as u16).max().unwrap_or(10).max(18) + 10;
            let width = width.min(area.width.saturating_sub(4));
            // The frame's two borders and its one row of top padding.
            let height = rows + 3;
            let rect = Rect::new(
                area.right().saturating_sub(width + 2),
                area.bottom().saturating_sub(height + 1),
                width,
                height.min(area.height.saturating_sub(2)),
            );
            let inner = modal_frame(f, rect, t, &format!("{here} · actions"));
            let lines: Vec<Line> = items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let active = i == *selected;
                    Line::from(vec![
                        Span::styled(if active { "▌" } else { " " }, Style::default().fg(t.focus)),
                        Span::styled(format!("{} ", item.key), t.strong_style().fg(t.focus)),
                        Span::styled(format!(" {}", item.label), if active { t.strong_style() } else { t.text_style() }),
                    ])
                })
                .collect();
            {
                let mut hits = app.hits.borrow_mut();
                hits.add(rect, Target::Swallow);
                hits.rows(super::super::hit::ListId::Sheet, inner, 0, items.len(), |_| None);
            }
            f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), inner);
        }
        Modal::GoTo => {
            // Every destination, grouped by section, one letter each: `g` then the letter.
            let features = app.config.features;
            let mut groups: Vec<(app::Section, Vec<(char, Screen)>)> = Vec::new();
            for (c, s) in super::super::keymap::ROUTES.iter().copied() {
                if !s.enabled(&features) {
                    continue;
                }
                let sec = s.section();
                match groups.iter_mut().find(|(g, _)| *g == sec) {
                    Some((_, v)) => v.push((c, s)),
                    None => groups.push((sec, vec![(c, s)])),
                }
            }
            groups.sort_by_key(|(s, _)| app::Section::ALL.iter().position(|x| x == s));
            let height = groups.len() as u16 + 3;
            let rect = Rect::new(area.x + 1, area.bottom().saturating_sub(height + 1), area.width.saturating_sub(2), height);
            let inner = modal_frame(f, rect, t, "go to · g then a letter · g g first row · esc");
            let mut hits = app.hits.borrow_mut();
            hits.add(rect, Target::Swallow);
            let mut lines = Vec::new();
            for (row, (sec, routes)) in groups.iter().enumerate() {
                let mut spans = vec![Span::styled(format!(" {:<9}", sec.title()), t.dim_style())];
                let mut x = inner.x + 10;
                for (c, s) in routes {
                    let cell = format!("{c} ");
                    let label = format!("{}   ", s.title());
                    let w = (cell.chars().count() + label.chars().count()) as u16;
                    hits.add(Rect::new(x, inner.y + row as u16, w.min(inner.right().saturating_sub(x)), 1), Target::Route(*s));
                    x += w;
                    spans.push(Span::styled(cell, t.strong_style().fg(t.focus)));
                    spans.push(Span::styled(label, t.text_style()));
                }
                lines.push(Line::from(spans));
            }
            drop(hits);
            f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), inner);
        }
        Modal::Themes(picker) => {
            let rect = centered(area, 116, 32);
            let inner = modal_frame(f, rect, t, "theme showroom · ↑↓ preview · type to filter · enter use · esc revert");
            let mut hits = app.hits.borrow_mut();
            hits.add(rect, Target::Swallow);
            draw_showroom(f, inner, t, picker, Some(&mut hits));
        }
        Modal::Wallets { selected } => {
            let selected = *selected;
            let here = app.meta.as_ref().map(|m| m.id.clone());
            let rows = app.wallets.len().max(1) as u16;
            let rect = centered(area, 64, rows + 6);
            let inner = modal_frame(f, rect, t, "wallets");
            let mut hits = app.hits.borrow_mut();
            hits.add(rect, Target::Swallow);
            let list = Rect { height: inner.height.saturating_sub(2), ..inner };
            hits.rows(super::super::hit::ListId::Wallets, list, 0, app.wallets.len(), |i| app.wallets.get(i).map(|w| w.id.clone()));
            let mut lines: Vec<Line> = app
                .wallets
                .iter()
                .enumerate()
                .map(|(i, w)| {
                    let current = Some(&w.id) == here.as_ref();
                    let kind = match w.kind {
                        wallet_core::registry::WalletKind::Watch => "watch-only",
                        _ => "keys",
                    };
                    let worth = app.wallet_summaries.get(&w.id).map(|s| amount::usd(s.total_usd)).unwrap_or_default();
                    let style = if i == selected { t.selected() } else { t.text_style() };
                    Line::from(vec![
                        Span::styled(if current { "▸ " } else { "  " }, Style::default().fg(t.focus)),
                        Span::styled(format!("{:<24}", truncate(&w.name, 24)), style.add_modifier(Modifier::BOLD)),
                        Span::styled(format!("{kind:<11}"), t.dim_style()),
                        Span::styled(format!("{worth:>12}"), t.text_style()),
                        Span::styled(if current { "  open" } else { "" }, t.dim_style()),
                    ])
                })
                .collect();
            if lines.is_empty() {
                lines.push(Line::from(Span::styled("No other wallets on this computer.", t.dim_style())));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("enter switches (this wallet locks first) · m manage · esc close", t.dim_style())));
            drop(hits);
            f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), inner);
        }
        Modal::TokenPicker { pay, query, selected } => {
            let (pay, query, selected) = (*pay, query.clone(), *selected);
            let rect = centered(area, 84, 22);
            let inner = modal_frame(f, rect, t, if pay { "you pay · pick a token" } else { "you receive · pick a token" });
            app.hits.borrow_mut().add(rect, Target::Swallow);
            super::super::views::draw_token_picker(f, app, t, inner, &query, selected, pay);
        }
        Modal::Effects(gallery) => {
            let rect = centered(area, 116, 32);
            let inner = modal_frame(f, rect, t, "lock screen gallery · ↑↓ preview · enter use · esc close");
            let mut hits = app.hits.borrow_mut();
            hits.add(rect, Target::Swallow);
            let mut lists = app.lists.borrow_mut();
            draw_gallery(f, inner, t, gallery, lists.entry(super::super::hit::ListId::Gallery).or_default(), &mut hits);
        }
    }
}

pub(crate) fn group4(s: &str) -> String {
    let body = s.strip_prefix("0x").unwrap_or(s);
    let groups: Vec<String> = body.chars().collect::<Vec<_>>().chunks(4).map(|c| c.iter().collect()).collect();
    if s.starts_with("0x") { format!("0x {}", groups.join(" ")) } else { groups.join(" ") }
}

pub(crate) fn draw_receive(f: &mut Frame, app: &mut App, t: &Theme, area: Rect, asset_qi: bool, account: usize) {
    let (data, subtitle) = if asset_qi {
        match app.meta.as_ref().and_then(|m| m.payment_code.clone()) {
            Some(code) => (code, "Payment code · every payment gets a fresh address · n new plain Qi address"),
            None => match app.dash.qi_addresses.last() {
                Some((_, a, _)) => (a.clone(), "Plain Qi address (reuse reduces privacy) · n new address"),
                None => (String::new(), "no Qi receive address yet · n new address"),
            },
        }
    } else {
        match app.dash.accounts.get(account) {
            Some(a) => (a.address.clone(), "Cyprus-1 · send only QUAI and Quai tokens here · j/k other account"),
            None => (String::new(), "no accounts yet"),
        }
    };
    let rect = centered(area, 76, area.height.saturating_sub(2).min(42));
    let inner = modal_frame(f, rect, t, "receive");
    let tab = |on: bool, color: Color| if on { t.chip(color) } else { t.dim_style() };
    let switch = Line::from(vec![
        Span::styled(" ", tab(!asset_qi, t.quai)),
        super::super::images::native_span(app, t, "quai"),
        Span::styled(" QUAI ", tab(!asset_qi, t.quai)),
        Span::raw(" "),
        Span::styled(" ", tab(asset_qi, t.qi)),
        super::super::images::native_span(app, t, "qi"),
        Span::styled(" Qi ", tab(asset_qi, t.qi)),
        Span::styled("   tab switch", t.dim_style()),
    ]);
    let label = app.dash.accounts.get(account).filter(|_| !asset_qi).map(|a| format!("{} · ", a.label)).unwrap_or_default();
    let text_lines = if data.starts_with("0x") { 1 } else { (data.len() as u16).div_ceil(inner.width.max(1)) };
    let [top, qr_area, text_area] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(5), Constraint::Length(text_lines + 3)]).areas(inner);
    {
        use super::super::hit::Button;
        let mut hits = app.hits.borrow_mut();
        hits.add(rect, Target::Swallow);
        // The switch is centered: its spans start where the centering puts the line.
        let w = switch.width() as u16;
        let row = Rect { x: top.x + top.width.saturating_sub(w) / 2, width: w.min(top.width), height: 1, ..top };
        hits.spans(row, &switch.spans, |i| match i {
            0..=2 => Some(Target::Button(Button::ReceiveAsset(false))),
            4..=6 => Some(Target::Button(Button::ReceiveAsset(true))),
            _ => None,
        });
        // The code or the address, and its QR: a click copies it.
        if !data.is_empty() {
            hits.add(qr_area, Target::Button(Button::CopyReceive));
            hits.add(Rect { height: text_lines, ..text_area }, Target::Button(Button::CopyReceive));
        }
    }
    f.render_widget(Paragraph::new(switch).alignment(Alignment::Center).style(Style::default().bg(t.raised)), top);
    let shown = if data.starts_with("0x") { group4(&data) } else { data.clone() };
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(shown, t.strong_style().fg(if asset_qi { t.qi } else { t.quai }))),
            Line::from(""),
            Line::from(Span::styled(format!("{label}{subtitle}"), t.dim_style())),
            Line::from(Span::styled("y copy · esc close", t.dim_style())),
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .style(Style::default().bg(t.raised)),
        text_area,
    );
    if data.is_empty() {
        return;
    }
    match app.caps.tier {
        // Inside tmux (placeholders) the QR is cells, which tmux keeps in place anyway.
        Tier::Pixels if !app.caps.placeholders => {
            let rows = qr_area.height.min(qr_area.width / 2).max(8);
            let cols = ((rows as u32 * app.caps.cell_px.1 as u32) / app.caps.cell_px.0.max(1) as u32) as u16;
            let cols = cols.min(qr_area.width);
            let r = Rect::new(qr_area.x + (qr_area.width - cols) / 2, qr_area.y, cols, rows.min(qr_area.height));
            app.qr_rect = Some((r, data));
        }
        Tier::Pixels | Tier::Cells => {
            let fit = [4usize, 2].into_iter().find_map(|quiet| {
                let (size, grid) = super::super::terminal::qr_modules(&data, quiet)?;
                ((size as u16) <= qr_area.width && (size.div_ceil(2) as u16) <= qr_area.height).then_some((size, grid))
            });
            match fit {
                Some((size, grid)) => paint_qr(f.buffer_mut(), qr_area, size, &grid),
                None => empty_state(f, qr_area, t, "▪", "Enlarge the terminal to show the QR code.", &[]),
            }
        }
        Tier::Text => empty_state(f, qr_area, t, "▪", "QR codes are off in text mode.", &[]),
    }
}

/// Half-block QR, always dark-on-white for scanners.
pub(crate) fn paint_qr(buf: &mut Buffer, area: Rect, size: usize, grid: &[bool]) {
    let h = size.div_ceil(2) as u16;
    let x0 = area.x + (area.width - size as u16) / 2;
    let y0 = area.y + (area.height - h) / 2;
    for row in 0..h as usize {
        for col in 0..size {
            let top = grid[row * 2 * size + col];
            let bottom = row * 2 + 1 < size && grid[(row * 2 + 1) * size + col];
            let ch = match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            };
            if let Some(cell) = buf.cell_mut((x0 + col as u16, y0 + row as u16)) {
                cell.set_char(ch).set_fg(Color::Black).set_bg(Color::White);
            }
        }
    }
}

/// Whether the current frame needs periodic redraws.
/// Whether something on screen moves continuously (a ceremony, a border drawing itself in, a
/// picture fading in, a flash): full frames at the frame rate. Spinners and the block pulse are
/// not in here; they redraw when they change (`spinning`, `BEAT_PULSE`), and the edge light is a
/// decoration frame of its own (`draw_edges`).
pub fn wants_animation(app: &App) -> bool {
    // Nothing turns for a window nobody is looking at: the 500 ms tick still redraws when a status
    // changes, and animation picks up again on focus.
    // The lock screen is the exception: a screensaver plays whether or not anyone is looking.
    let lock_screen = app.locked && (app.ambient.is_some() || app.lock_fade.is_some());
    (app.focused || lock_screen) && app.motion() != Motion::Off && (app.animating() || app.eco.fading())
}

/// Whether a spinner is showing that should turn: the status line's while work runs, and the
/// pending pill's while a transaction waits to be mined.
pub fn spinning(app: &App) -> bool {
    app.focused && (app.spun || app.busy_label().is_some() || app.unlocking || (app.motion().effects() && !app.confirming_ops().is_empty()))
}
