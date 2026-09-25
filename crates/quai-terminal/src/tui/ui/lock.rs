//! The lock screen and onboarding.

use super::*;

// ---------------------------------------------------------------- lock / onboarding

/// How long a finished effect's last frame takes to dissolve into the resting wordmark.
pub(crate) const HANDOVER_MS: u128 = 500;

pub(crate) fn draw_lock(f: &mut Frame, app: &mut App, t: &Theme, area: Rect) {
    // The effect canvas is built for exactly this art area (see `app::lock_art_size`).
    let (_, art_h) = app::lock_art_size((area.width, area.height));
    // `centered` keeps a row of margin above and below, so the band has to be two taller than
    // the card itself — at 9 the card lost its last line, which is the line that speaks.
    let [_, art, form, _] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(art_h), Constraint::Length(11), Constraint::Min(0)]).areas(area);
    let accent = t.accent_rgb.map(|(r, g, b)| Color::Rgb(r, g, b)).unwrap_or(t.focus);
    // An effect plays when the wallet locks. With `lock_loop` on (the default) the next starts the
    // moment one ends, in the background too; off, the screen rests after one, since effects
    // chained forever hold about a fifth of a core. Either way it goes still the moment a
    // password is being typed, and picks up again (`App::tick`) once the field is empty.
    let typing = !app.lock.input.is_empty() || app.lock.unlocking;
    let last_frame = |app: &App| app.fx.ambient.as_ref().and_then(|c| c.frame()).map(|f| (f.to_string(), std::time::Instant::now()));
    let rest = |app: &mut App| {
        app.lock.fade = last_frame(app);
        app.fx.ambient = None;
        app.lock.rested = true;
    };
    if let Some(c) = app.fx.ambient.as_mut() {
        if typing {
            rest(app);
        } else if !c.advance() {
            // Looping: the next effect starts in this very frame, with the one that just ended
            // dissolving over it, so there is no held last frame between them. Whether or not
            // the window has the focus: the lock screen is a screensaver.
            let ended = app.fx.ambient.as_ref().and_then(|c| c.frame()).map(str::to_string);
            app.fx.ambient = None;
            if app.config.lock_loop {
                app.start_lock_ceremony((area.width, area.height));
            }
            // Timed from here, once the next effect exists: building it can take longer than the
            // dissolve on a slow machine, which would use the whole handover up before a frame
            // of it was drawn.
            app.lock.fade = ended.map(|frame| (frame, std::time::Instant::now()));
            if app.fx.ambient.is_none() {
                app.lock.rested = true;
            }
        }
    }
    if let Some(c) = &app.fx.ambient {
        c.render(art, f.buffer_mut(), t.base().fg(accent));
        // The effect that just ended thins away on top of the one that began.
        if let Some((frame, at)) = app.lock.fade.take() {
            let ms = at.elapsed().as_millis();
            if ms < HANDOVER_MS {
                let keep = 1.0 - ms as f32 / HANDOVER_MS as f32;
                super::super::fx::paint_dissolve(&frame, art, f.buffer_mut(), t.base().fg(accent), keep);
                app.lock.fade = Some((frame, at));
            }
        }
    } else {
        // Resting (or effects off): the wordmark, with the chain line under it.
        wordmark(f, art, t);
        if let Some(line) = app.chain_weather() {
            f.render_widget(
                Paragraph::new(Span::styled(line, t.dim_style())).alignment(Alignment::Center),
                Rect { y: art.bottom().saturating_sub(1), height: 1, ..art },
            );
        }
        // The effect's last frame thins away over the resting wordmark, so the end is a
        // cross-fade rather than a cut. Typing cuts it short: nothing moves near a password.
        if let Some((frame, at)) = app.lock.fade.take() {
            let ms = at.elapsed().as_millis();
            if ms < HANDOVER_MS && !typing {
                let keep = 1.0 - ms as f32 / HANDOVER_MS as f32;
                super::super::fx::paint_dissolve(&frame, art, f.buffer_mut(), t.base().fg(accent), keep);
                app.lock.fade = Some((frame, at));
            }
        }
    }
    let rect = centered(form, 58, 8);
    let inner = modal_frame(f, rect, t, "locked");
    let name = app.meta.as_ref().map(|m| m.name.clone()).unwrap_or_default();
    let dots = "•".repeat(app.lock.input.chars().count().min(40));
    let lines = vec![
        Line::from(vec![
            super::super::images::native_span(app, t, "quai"),
            Span::raw(" "),
            Span::styled(name, t.strong_style()),
            Span::styled(format!("  ·  {}", app.network_id), t.dim_style()),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("password  ", t.dim_style()),
            Span::styled(dots, t.strong_style()),
            Span::styled("▏", Style::default().fg(t.focus)),
        ]),
        password_rule(app, t, inner.width.saturating_sub(10) as usize).alignment(Alignment::Right),
        // What this screen is doing, in its own words: the unlock it was asked for beats any
        // background work, and an answer that came back beats the hint.
        Line::from(match (app.lock.unlocking, &app.lock.error) {
            (true, _) => Span::styled(format!("{} unlocking…", spinner()), Style::default().fg(t.pending)),
            (false, Some(e)) => Span::styled(format!("{} {e}", t.icon(Icon::Danger)), Style::default().fg(t.danger)),
            (false, None) if app.cockpit.list.len() > 1 => {
                Span::styled("enter unlock · ctrl-w another wallet · esc clear · ctrl-c quit", t.dim_style())
            }
            (false, None) => Span::styled("enter unlock · esc clear · ctrl-c quit", t.dim_style()),
        }),
    ];
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), inner);
}

/// The rule under the password. While the key is being derived a short light travels along it
/// (the key turning), inside the card and never behind what was typed; a wrong password turns
/// it red and it holds still. Nothing here ever shakes.
fn password_rule(app: &App, t: &Theme, width: usize) -> Line<'static> {
    const LAP_MS: u128 = 900;
    const LIGHT: usize = 6;
    if app.lock.error.is_some() {
        return Line::from(Span::styled("▔".repeat(width), Style::default().fg(t.danger)));
    }
    let lit = app.lock.unlocking && app.motion().effects() && width > LIGHT;
    let Some(since) = app.lock.unlocking_since.filter(|_| lit) else {
        return Line::from(Span::styled(
            "▔".repeat(width),
            if app.lock.unlocking { Style::default().fg(t.pending) } else { t.dim_style() },
        ));
    };
    let head = (since.elapsed().as_millis() % LAP_MS) as usize * (width + LIGHT) / LAP_MS as usize;
    let start = head.saturating_sub(LIGHT).min(width);
    let end = head.min(width);
    Line::from(vec![
        Span::styled("▔".repeat(start), t.dim_style()),
        Span::styled("▔".repeat(end - start), Style::default().fg(t.focus)),
        Span::styled("▔".repeat(width - end), t.dim_style()),
    ])
}

pub(crate) fn wordmark(f: &mut Frame, area: Rect, t: &Theme) {
    let block = super::super::fx::wordmark_block();
    let lines: Vec<Line> = block
        .lines()
        .enumerate()
        .map(|(i, l)| {
            Line::from(Span::styled(l.to_string(), Style::default().fg(if i < 2 { t.quai } else { t.qi }).add_modifier(Modifier::BOLD)))
        })
        .collect();
    let h = lines.len() as u16;
    let rect = Rect::new(area.x, area.y + area.height.saturating_sub(h) / 2, area.width, h.min(area.height));
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), rect);
}

/// An onboarding frame that takes the edge light, as a focused panel does: the first screens a
/// person sees carry the same light the wallet does. The recovery phrase and its quiz keep a
/// plain frame; a secret is read, not watched.
fn lit_frame(f: &mut Frame, rect: Rect, t: &Theme, title: &str) -> Rect {
    let inner = modal_frame(f, rect, t, title);
    if let Some(corner) = f.buffer_mut().cell_mut((rect.x, rect.y)) {
        corner.modifier.insert(Modifier::BOLD);
    }
    inner
}

pub(crate) fn steps_line(t: &Theme, current: usize) -> Line<'static> {
    let names = ["look", "privacy", "connections", "wallet", "protect"];
    let mut spans = Vec::new();
    for (i, n) in names.iter().enumerate() {
        if i > 0 {
            // A finished step closes its connector: the line lights as the circuit completes.
            let joined = i < current;
            spans.push(Span::styled(" ── ", if joined { Style::default().fg(t.ok) } else { t.dim_style() }));
        }
        let step = i + 1;
        let style = if step == current {
            t.strong_style().fg(t.focus)
        } else if step < current {
            Style::default().fg(t.ok)
        } else {
            t.dim_style()
        };
        let mark = if step < current { t.icon(Icon::Ok).to_string() } else { step.to_string() };
        spans.push(Span::styled(format!("{mark} {n}"), style));
    }
    Line::from(spans)
}

pub(crate) fn draw_onboarding(f: &mut Frame, app: &mut App, t: &Theme, area: Rect) {
    let Some(ob) = app.onboarding.as_ref() else { return };
    let step = super::super::onboarding::step(ob);
    let [top, body, bottom] = Layout::vertical([Constraint::Length(2), Constraint::Min(10), Constraint::Length(1)]).areas(area);
    // The welcome stands alone; the steps begin after it.
    if step > 0 {
        f.render_widget(Paragraph::new(vec![Line::from(""), steps_line(t, step)]).alignment(Alignment::Center), top);
    }
    let hint: String = match ob {
        Onboarding::Welcome => "enter begin · ctrl-c quit".into(),
        Onboarding::Theme(_) => "↑↓ preview · type to filter · enter choose · esc keep current".into(),
        Onboarding::Motion { .. } => "↑↓ try one · enter choose · esc back to themes · change any time in System › Settings".into(),
        Onboarding::Privacy { .. } => "↑↓ choose · enter continue · esc back · change any time in System › Data sources".into(),
        Onboarding::Connections { .. } => {
            "tab/↓ next · enter continue (empty keeps the default) · ctrl-u clear · esc back · change any time in Settings".into()
        }
        Onboarding::Choose { .. } => "↑↓ choose · enter continue · n network · t themes · ctrl-c quit".into(),
        Onboarding::ShowPhrase { .. } => "enter I wrote it down · esc start over".into(),
        Onboarding::Quiz { .. } => "tab next word · enter check · esc show the phrase again · ctrl-s skip (not recommended)".into(),
        Onboarding::Details { .. } => "tab/↓ next field · enter continue · ctrl-u clear · esc back".into(),
    };
    f.render_widget(Paragraph::new(Span::styled(hint, t.dim_style())).alignment(Alignment::Center), bottom);
    match ob {
        Onboarding::Welcome => {
            let rect = centered(body, body.width, 15);
            let [mark, rest] = Layout::vertical([Constraint::Length(6), Constraint::Min(1)]).areas(rect);
            wordmark(f, mark, t);
            let lines = vec![
                Line::from(vec![
                    Span::styled("A wallet for ", t.text_style()),
                    Span::styled("QUAI", t.strong_style().fg(t.quai)),
                    Span::styled(" and ", t.text_style()),
                    Span::styled("Qi", t.strong_style().fg(t.qi)),
                    Span::styled(" that lives in your terminal.", t.text_style()),
                ]),
                Line::from(Span::styled("Your keys stay on this computer, locked with a password only you know.", t.dim_style())),
                Line::from(""),
                Line::from(Span::styled("A few questions first: how it looks, what it may share, and which wallet.", t.dim_style())),
                Line::from(Span::styled("Every answer can be changed later.", t.dim_style())),
                Line::from(""),
                Line::from(super::super::widgets::button(t, "begin", "enter", t.focus, super::super::widgets::ButtonState::Focused)),
            ];
            f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), rest);
        }
        Onboarding::Motion { selected, .. } => {
            let rect = centered(body, 92, 15);
            let inner = lit_frame(f, rect, t, "motion");
            let mut lines = vec![
                Line::from(Span::styled("How much should move?", t.strong_style())),
                Line::from(Span::styled("Each one plays as you land on it. Over SSH it never goes past Reduced.", t.dim_style())),
                Line::from(""),
            ];
            for (i, (_, label, sub)) in super::super::onboarding::MOTIONS.iter().enumerate() {
                let active = i == *selected;
                lines.push(Line::from(vec![
                    Span::styled(if active { "▌ " } else { "  " }, Style::default().fg(t.focus)),
                    Span::styled(format!("{label:<10}"), if active { t.strong_style().fg(t.focus) } else { t.text_style() }),
                    Span::styled(*sub, t.dim_style()),
                ]));
                lines.push(Line::from(""));
            }
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).style(Style::default().bg(t.raised)), inner);
        }
        Onboarding::Theme(picker) => {
            let rect = centered(body, 112, 30);
            let inner = lit_frame(f, rect, t, "pick a look · you can change it any time with T");
            draw_showroom(f, inner, t, picker, None);
        }
        Onboarding::Privacy { selected } => {
            let rect = centered(body, 100, 22);
            let inner = lit_frame(f, rect, t, "privacy · the explorer");
            let width = usize::from(inner.width).max(20);
            let wrap = super::super::views::board::wrap_words;
            // Every answer's cost stays on screen whatever the size: the spacing goes first.
            let build = |spaced: bool| {
                let mut lines = vec![Line::from(Span::styled("Should explorer.qu.ai look up your addresses?", t.strong_style()))];
                for part in wrap(super::super::onboarding::PRIVACY_INTRO, width, width) {
                    lines.push(Line::from(Span::styled(part, t.dim_style())));
                }
                if spaced {
                    lines.push(Line::from(""));
                }
                let room = width.saturating_sub(20).max(20);
                for (i, choice) in super::super::onboarding::PRIVACY.iter().enumerate() {
                    let active = i == *selected;
                    let body = if active { t.text_style() } else { t.dim_style() };
                    lines.push(Line::from(vec![
                        Span::styled(if active { "▌ " } else { "  " }, Style::default().fg(t.focus)),
                        Span::styled(format!("{:<11}", choice.label), if active { t.strong_style().fg(t.focus) } else { t.text_style() }),
                        Span::styled(choice.says, body),
                    ]));
                    // Wrapped under their own column, not back at the margin.
                    for (tag, text) in [("gets", choice.gets), ("costs", choice.costs)] {
                        for (n, part) in wrap(text, room, room).into_iter().enumerate() {
                            let tag = if n == 0 { tag } else { "" };
                            lines.push(Line::from(vec![
                                Span::styled(format!("{:13}{tag:<7}", ""), t.dim_style()),
                                Span::styled(part, body),
                            ]));
                        }
                    }
                    if spaced {
                        lines.push(Line::from(""));
                    }
                }
                lines
            };
            let full = build(true);
            let lines = if full.len() <= usize::from(inner.height) { full } else { build(false) };
            f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), inner);
        }
        Onboarding::Connections { fields, focus } => {
            let rect = centered(body, 104, 32);
            let inner = lit_frame(f, rect, t, "connections · every one of these has a default that works");
            let width = usize::from(inner.width).max(20);
            let wrap = super::super::views::board::wrap_words;
            let node = fields.first().map(|f| f.value.as_str()).unwrap_or_default();
            let rpc = app.config.network(&app.network_id).map(|n| n.rpc_url).unwrap_or_default();
            let routes = super::super::onboarding::routes(node, app.config.explorer_lookups, &rpc);
            // Where every request goes stays on screen whatever the size: spacing goes first, then
            // the notes under each host.
            let build = |spaced: bool, notes: bool| {
                let mut lines = vec![Line::from(Span::styled("Where this wallet reads from.", t.strong_style()))];
                if spaced {
                    lines.push(Line::from(Span::styled("Press enter through them all to take the defaults.", t.dim_style())));
                    lines.push(Line::from(""));
                }
                for (i, field) in fields.iter().enumerate() {
                    let active = i == *focus;
                    let shown = if field.value.is_empty() { field.hint.clone() } else { field.value.clone() };
                    let style = if field.value.is_empty() { t.dim_style() } else { t.text_style() };
                    lines.push(Line::from(vec![
                        Span::styled(if active { "▌ " } else { "  " }, Style::default().fg(t.focus)),
                        Span::styled(format!("{:<18}", field.label), if active { t.strong_style().fg(t.focus) } else { t.text_style() }),
                        Span::styled(shown, style),
                        Span::styled(if active { "▏" } else { "" }, Style::default().fg(t.focus)),
                    ]));
                }
                if spaced {
                    lines.push(Line::from(""));
                }
                // Why the field under the cursor is worth setting, in front of the person deciding.
                if let Some((_, why)) = super::super::onboarding::CONNECTIONS.get(*focus) {
                    for part in wrap(why, width, width) {
                        lines.push(Line::from(Span::styled(part, t.dim_style())));
                    }
                }
                // What these settings add up to, as they are typed: which host gets which request.
                if spaced {
                    lines.push(Line::from(""));
                }
                lines.push(Line::from(Span::styled("what goes where", t.strong_style())));
                let room = width.saturating_sub(20).max(20);
                for (what, host, note) in &routes {
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {what:<17}"), t.dim_style()),
                        Span::styled(host.clone(), t.text_style()),
                    ]));
                    if notes {
                        for part in wrap(note, room, room) {
                            lines.push(Line::from(Span::styled(format!("  {:<17}{part}", ""), t.dim_style())));
                        }
                    }
                }
                lines
            };
            let height = usize::from(inner.height);
            let lines = [(true, true), (false, true), (false, false)]
                .into_iter()
                .map(|(spaced, notes)| build(spaced, notes))
                .find(|lines| lines.len() <= height)
                .unwrap_or_else(|| build(false, false));
            f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), inner);
        }
        Onboarding::Choose { selected } => {
            let rect = centered(body, 86, 18);
            let inner = lit_frame(f, rect, t, "welcome");
            let [mark, rest] = Layout::vertical([Constraint::Length(5), Constraint::Min(4)]).areas(inner);
            wordmark(f, mark, t);
            let mut lines = vec![
                Line::from(vec![
                    Span::raw("A self-custodial wallet for "),
                    Span::styled("QUAI", t.strong_style().fg(t.quai)),
                    Span::raw(" and "),
                    Span::styled("Qi", t.strong_style().fg(t.qi)),
                    Span::raw(", right in your terminal."),
                ]),
                Line::from(""),
            ];
            for (i, (label, sub)) in super::super::onboarding::CHOICES.iter().enumerate() {
                let active = i == *selected;
                lines.push(Line::from(vec![
                    Span::styled(if active { "▌ " } else { "  " }, Style::default().fg(t.focus)),
                    Span::styled(format!("{label:<28}"), if active { t.strong_style().fg(t.focus) } else { t.text_style() }),
                    Span::styled(*sub, t.dim_style()),
                ]));
            }
            lines.push(Line::from(""));
            let mainnet = app.network_id == "mainnet";
            lines.push(Line::from(vec![
                Span::styled("network  ", t.dim_style()),
                Span::styled(app.network_id.clone(), if mainnet { Style::default().fg(t.link) } else { t.strong_style().fg(t.attention) }),
                Span::styled(if mainnet { "  · real funds · n to change" } else { "  · n to change" }, t.dim_style()),
            ]));
            lines.push(Line::from(vec![
                Span::styled("stored   ", t.dim_style()),
                Span::styled(app::short_path(&app.paths.root().display().to_string()), t.dim_style()),
            ]));
            f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), rest);
        }
        Onboarding::ShowPhrase { phrase } => {
            let rect = centered(body, 86, phrase.split_whitespace().count().div_ceil(4) as u16 + 10);
            let inner = modal_frame(f, rect, t, "your recovery phrase");
            let mut lines = vec![
                Line::from(Span::styled("Write these 24 words down, in order, on paper.", t.strong_style())),
                Line::from(Span::styled(
                    "Anyone who has them controls your funds. Never type them into a website or share a photo.",
                    Style::default().fg(t.attention),
                )),
                Line::from(""),
            ];
            let words: Vec<&str> = phrase.split_whitespace().collect();
            let rows = words.len().div_ceil(4);
            for r in 0..rows {
                let mut spans = Vec::new();
                for c in 0..4 {
                    // Column-first numbering: 1–6 down the first column.
                    if let Some(w) = words.get(c * rows + r) {
                        spans.push(Span::styled(format!("{:>4} ", c * rows + r + 1), t.dim_style()));
                        spans.push(Span::styled(format!("{w:<14}"), t.strong_style()));
                    }
                }
                lines.push(Line::from(spans));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Next you'll confirm three of the words.", t.dim_style())));
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).style(Style::default().bg(t.raised)), inner);
        }
        Onboarding::Quiz { indexes, answers, focus, .. } => {
            let rect = centered(body, 70, 12);
            let inner = modal_frame(f, rect, t, "confirm your backup");
            let mut lines = vec![Line::from(Span::styled("Type these words from your written copy:", t.text_style())), Line::from("")];
            for i in 0..3 {
                let active = i == *focus;
                lines.push(Line::from(vec![
                    Span::styled(if active { "▌ " } else { "  " }, Style::default().fg(t.focus)),
                    Span::styled(format!("word #{:<3}  ", indexes[i] + 1), t.dim_style()),
                    Span::styled(
                        format!("{}{}", answers[i], if active { "▏" } else { "" }),
                        if active { t.strong_style().fg(t.focus) } else { t.text_style() },
                    ),
                ]));
            }
            f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), inner);
        }
        Onboarding::Details { kind, fields, focus, verified, .. } => {
            let heading = match kind {
                OnboardKind::Create => "Name and protect your new wallet",
                OnboardKind::ImportPhrase => "Import a recovery phrase",
                OnboardKind::ImportKey => "Import a private key",
                OnboardKind::Watch => "Watch addresses",
            };
            let rect = centered(body, 84, fields.len() as u16 * 3 + 9);
            let inner = lit_frame(f, rect, t, heading);
            let mut lines = Vec::new();
            if *kind == OnboardKind::Create && !verified {
                lines.push(Line::from(Span::styled(
                    "! Phrase not verified. You can verify later: quai-terminal wallet verify-phrase",
                    Style::default().fg(t.attention),
                )));
                lines.push(Line::from(""));
            }
            let _ = push_fields(&mut lines, t, fields, *focus, None, None, inner.width);
            if let Some(b) = &app.status.busy {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(format!("{} {b}", spinner()), Style::default().fg(t.pending))));
            }
            f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), inner);
        }
    }
}

/// Theme showroom: grouped list with swatches, live mock-up preview, contrast badge.
pub(crate) fn draw_showroom(f: &mut Frame, area: Rect, t: &Theme, picker: &Picker, mut hits: Option<&mut super::super::hit::HitMap>) {
    let [list_area, preview] = Layout::horizontal([Constraint::Length(40), Constraint::Min(30)]).areas(area);
    let visible = picker.visible();
    let mut lines: Vec<Line> = vec![Line::from(vec![
        Span::styled("/ ", t.dim_style()),
        Span::styled(
            if picker.filter.is_empty() { "type to filter".to_string() } else { format!("{}▏", picker.filter) },
            if picker.filter.is_empty() { t.dim_style() } else { t.strong_style() },
        ),
    ])];
    let height = list_area.height.saturating_sub(2) as usize;
    let sel_pos = visible.iter().position(|&i| i == picker.selected).unwrap_or(0);
    let start = sel_pos.saturating_sub(height.saturating_sub(4));
    let mut family = String::new();
    for (pos, &i) in visible.iter().enumerate().skip(start) {
        let e = &picker.entries[i];
        if e.family != family {
            family = e.family.clone();
            lines.push(Line::from(Span::styled(format!("  {}", family.to_lowercase()), t.dim_style().add_modifier(Modifier::ITALIC))));
        }
        let active = i == picker.selected;
        let sw = |c: Color| Span::styled("■", Style::default().fg(c).bg(e.theme.surface));
        let mut spans = vec![
            Span::styled(if active { "▌" } else { " " }, Style::default().fg(t.focus)),
            Span::styled(format!(" {:<22}", truncate(&e.name, 22)), if active { t.strong_style().fg(t.focus) } else { t.text_style() }),
            Span::styled(" ", Style::default().bg(e.theme.surface)),
        ];
        for c in [e.theme.text, e.theme.focus, e.theme.quai, e.theme.qi, e.theme.ok, e.theme.danger] {
            spans.push(sw(c));
        }
        spans.push(Span::styled(" ", Style::default().bg(e.theme.surface)));
        // A click previews that theme, the same as moving onto it.
        if let Some(h) = hits.as_deref_mut()
            && (lines.len() as u16) < list_area.height
        {
            let row = Rect::new(list_area.x, list_area.y + lines.len() as u16, list_area.width, 1);
            h.add(row, Target::Row { list: super::super::hit::ListId::Themes, index: pos, key: None });
        }
        lines.push(Line::from(spans));
        if lines.len() > height {
            break;
        }
    }
    if visible.is_empty() {
        lines.push(Line::from(Span::styled("  no theme matches", t.dim_style())));
    }
    f.render_widget(Paragraph::new(lines), list_area);

    // Live preview in the candidate theme (already applied to `t`).
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(t.border(false))
        .style(t.base())
        .padding(Padding::new(2, 2, 1, 0));
    let inner = block.inner(preview);
    f.render_widget(block, preview);
    let name = picker.current().map(|e| e.name.clone()).unwrap_or_default();
    let contrast = t.text_contrast();
    let badge = match contrast {
        Some(c) if c >= 7.0 => Span::styled(format!("{} contrast {c:.1}:1 AAA", t.icon(Icon::Ok)), Style::default().fg(t.ok)),
        Some(c) => Span::styled(format!("{} contrast {c:.1}:1 AA", t.icon(Icon::Ok)), Style::default().fg(t.ok)),
        None => Span::styled("follows your terminal colors", t.dim_style()),
    };
    let digits = big_digits("1,204");
    let mut lines = vec![
        Line::from(vec![Span::styled(name, t.strong_style().fg(t.focus)), Span::raw("   "), badge]),
        Line::from(if t.adjusted {
            Span::styled("! some colors were adjusted for readability", Style::default().fg(t.attention))
        } else {
            Span::raw("")
        }),
        Line::from(vec![Span::styled("▌", Style::default().fg(t.quai)), Span::styled(" QUAI", t.dim_style())]),
    ];
    for (i, row) in digits.iter().enumerate() {
        lines.push(Line::from(vec![
            Span::styled("▌ ", Style::default().fg(t.quai)),
            Span::styled(row.clone(), t.strong_style().fg(t.quai)),
            Span::styled(if i == 2 { " .5183" } else { "" }, t.dim_style()),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled("▌", Style::default().fg(t.qi)),
        Span::styled(" Qi ", t.dim_style()),
        Span::styled("386.286", t.strong_style().fg(t.qi)),
        Span::styled(format!("   {} 3.416 locked", t.icon(Icon::Locked)), Style::default().fg(t.pending)),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(format!("{} confirmed  ", t.icon(Icon::Ok)), Style::default().fg(t.ok)),
        Span::styled(format!("{} pending  ", t.icon(Icon::InFlight)), Style::default().fg(t.pending)),
        Span::styled("↩ refunded  ", Style::default().fg(t.attention)),
        Span::styled(format!("{} failed", t.icon(Icon::Danger)), Style::default().fg(t.danger)),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" 2m ", t.dim_style()),
        Span::styled("↘ ", Style::default().fg(t.qi)),
        Span::raw("received 12.5 Qi → 0x00F4…804B"),
    ]));
    lines.push(Line::from(Span::styled(" 5m ↔ QUAI→Qi conversion of 100 QUAI", t.selected())));
    lines.push(Line::from(vec![
        Span::styled(" 9m ", t.dim_style()),
        Span::styled("↗ ", Style::default().fg(t.quai)),
        Span::raw("send of 1.25 QUAI to "),
        Span::styled("bob", Style::default().fg(t.link)),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  Reject  ", t.selected()),
        Span::raw("   "),
        Span::styled("  Approve & sign  ", t.chip(t.ok)),
    ]));
    f.render_widget(Paragraph::new(lines), inner);
    let spark_area = Rect::new(inner.x, inner.bottom().saturating_sub(2), inner.width.min(40), 2);
    if spark_area.y > inner.y + 16 {
        f.render_widget(
            Sparkline::default()
                .data([3u64, 4, 3, 5, 6, 5, 7, 6, 8, 7, 9, 8, 7, 9, 10, 9, 11, 10, 12, 11])
                .style(Style::default().fg(t.focus)),
            spark_area,
        );
    }
}

/// Looks up "available X" for an amount field's asset.
pub(crate) type Available<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// Effect list with a live, looping preview of the selection on the lock screen wordmark.
pub(crate) fn draw_gallery(
    f: &mut Frame,
    area: Rect,
    t: &Theme,
    g: &mut app::Gallery,
    state: &mut super::super::hit::ListState,
    hits: &mut super::super::hit::HitMap,
) {
    let [list, preview] = Layout::horizontal([Constraint::Length(24), Constraint::Min(30)]).areas(area);
    let names: Vec<&str> = std::iter::once("random").chain(super::super::fx::EFFECTS.iter().map(|(n, _)| *n)).collect();
    let height = list.height as usize;
    let start = state.window(g.selected, names.len(), height);
    hits.rows(super::super::hit::ListId::Gallery, list, start, names.len(), |_| None);
    let lines: Vec<Line> = names
        .iter()
        .enumerate()
        .skip(start)
        .take(height)
        .map(|(i, n)| {
            let active = i == g.selected;
            Line::from(vec![
                Span::styled(if active { "▌ " } else { "  " }, Style::default().fg(t.focus)),
                Span::styled(
                    if i == 0 { "random (default)".to_string() } else { (*n).to_string() },
                    if active { t.strong_style().fg(t.focus) } else { t.text_style() },
                ),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), list);

    let [info, stage] = Layout::vertical([Constraint::Length(3), Constraint::Min(6)]).areas(preview);
    let description = if g.selected == 0 {
        "a different effect each time the screen locks".to_string()
    } else {
        super::super::fx::EFFECTS[g.selected - 1].1.to_string()
    };
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(g.value(), t.strong_style().fg(t.focus))),
            Line::from(Span::styled(description, t.dim_style())),
        ]),
        info,
    );
    let stage_block = Block::default().borders(Borders::ALL).border_type(BorderType::Plain).border_style(t.border(false)).style(t.base());
    let canvas = stage_block.inner(stage);
    f.render_widget(stage_block, stage);
    // Loop the way the lock screen does: the preview restarts the instant it ends, so what the
    // gallery shows is what you get — no still wordmark sitting between runs.
    let wanted = if g.selected == 0 { None } else { Some(super::super::fx::EFFECTS[g.selected - 1].0) };
    let stale = g.preview.as_ref().is_some_and(|c| c.size() != (canvas.width, canvas.height));
    if stale {
        g.preview = None;
    }
    if g.preview.as_mut().is_none_or(|c| !c.advance()) {
        let name = wanted.map(str::to_string).unwrap_or_else(|| super::super::fx::random_lock_effect().to_string());
        let args = super::super::fx::theme_args(&name, t);
        g.preview =
            super::super::fx::Ceremony::with_args(&name, &args, &super::super::fx::wordmark_block(), canvas.width, canvas.height, 900);
        // First step now: a ceremony carries no frame until it is stepped.
        if let Some(c) = g.preview.as_mut() {
            c.step();
        }
    }
    if let Some(c) = &g.preview {
        let accent = t.accent_rgb.map(|(r, g, b)| Color::Rgb(r, g, b)).unwrap_or(t.focus);
        c.render(canvas, f.buffer_mut(), t.base().fg(accent));
    } else {
        wordmark(f, canvas, t);
    }
}

/// Form fields with an underline track, inline error, available-balance hint and strength meter.
/// Draw the fields, and report the line range the focused one occupies.
///
/// Only this knows it: a field is two lines, or three when it carries an error or a hint, so
/// anything computing it from the outside would drift the moment that changed. A form long enough
/// to scroll needs the range to keep the cursor on screen.
pub(crate) fn push_fields(
    lines: &mut Vec<Line<'static>>,
    t: &Theme,
    fields: &[app::Field],
    focus: usize,
    error: Option<(usize, &str)>,
    available: Option<&Available>,
    width: u16,
) -> std::ops::Range<usize> {
    let mut focused = 0..0;
    let label_w = fields.iter().map(|f| f.label.chars().count()).max().unwrap_or(10).max(10) + 2;
    let track_w = (width as usize).saturating_sub(label_w + 2).min(64);
    for (i, field) in fields.iter().enumerate() {
        let active = i == focus;
        let started = lines.len();
        let value = match &field.kind {
            k if field.is_secret() && *k != FieldKind::Text => "•".repeat(field.value.chars().count()),
            FieldKind::Choice(_) => format!("‹ {} ›", field.choice_label().unwrap_or(&field.value)),
            FieldKind::Amount(_) => super::super::num::typed(&field.value),
            _ => field.value.clone(),
        };
        let shown = if value.chars().count() > track_w {
            format!("…{}", value.chars().rev().take(track_w.saturating_sub(2)).collect::<Vec<_>>().into_iter().rev().collect::<String>())
        } else {
            value
        };
        let label_style = if active { t.strong_style().fg(t.focus) } else { t.dim_style() };
        let mut spans = vec![
            Span::styled(if active { "▌" } else { " " }, Style::default().fg(t.focus)),
            Span::styled(format!("{:<label_w$}", field.label), label_style),
            Span::styled(
                format!("{shown}{}", if active && !matches!(field.kind, FieldKind::Choice(_)) { "▏" } else { "" }),
                if active { t.strong_style() } else { t.text_style() },
            ),
        ];
        // The hint is a placeholder on the input line, so it can't be mistaken for the next field.
        if field.value.is_empty() && !matches!(field.kind, FieldKind::Choice(_)) {
            let placeholder = match (field.hint.is_empty(), field.optional) {
                (true, true) => "optional".to_string(),
                (true, false) => String::new(),
                (false, true) => format!("{} · optional", field.hint),
                (false, false) => field.hint.clone(),
            };
            spans.push(Span::styled(truncate(&placeholder, track_w.saturating_sub(1)), t.dim_style().add_modifier(Modifier::ITALIC)));
        }
        lines.push(Line::from(spans));
        let track_style = if error.is_some_and(|(e, _)| e == i) {
            Style::default().fg(t.danger)
        } else if active {
            Style::default().fg(t.focus)
        } else {
            Style::default().fg(t.line_strong)
        };
        let mut under = vec![Span::raw(" ".repeat(label_w + 1)), Span::styled("▔".repeat(track_w), track_style)];
        if let FieldKind::NewSecret = field.kind
            && !field.value.is_empty()
        {
            let s = app::password_strength(&field.value);
            let (label, color) =
                [("too short", t.danger), ("weak", t.danger), ("fair", t.attention), ("good", t.ok), ("strong", t.ok)][s as usize];
            under = vec![
                Span::raw(" ".repeat(label_w + 1)),
                Span::styled("█".repeat(s as usize + 1), Style::default().fg(color)),
                Span::styled("░".repeat(4 - s as usize), t.dim_style()),
                Span::styled(format!(" {label}"), Style::default().fg(color)),
            ];
        }
        lines.push(Line::from(under));
        match error {
            Some((e, msg)) if e == i => lines.push(Line::from(vec![
                Span::raw(" ".repeat(label_w + 1)),
                Span::styled(format!("{} {msg}", t.icon(Icon::Danger)), Style::default().fg(t.danger)),
            ])),
            _ => {
                let mut info = String::new();
                if let (FieldKind::Amount(asset), Some(avail)) = (&field.kind, available)
                    && let Some(a) = avail(asset)
                {
                    info = format!("available {a}");
                } else if !field.value.is_empty() && matches!(field.kind, FieldKind::Text) && active {
                    info = field.hint.clone();
                }
                if active && !info.is_empty() {
                    lines.push(Line::from(vec![Span::raw(" ".repeat(label_w + 1)), Span::styled(info, t.dim_style())]));
                } else {
                    lines.push(Line::from(""));
                }
            }
        }
        if active {
            focused = started..lines.len();
        }
    }
    focused
}
