//! People › Board: channels, sealed conversations and the pinned chat.

use super::*;
use crate::tui::ui::hint_line;

// ---------------------------------------------------------------- People › Board

/// The on-chain message board: the channels this wallet follows, and the open one's messages
/// oldest first, the way a conversation reads. Every body was written by a stranger, so it is
/// shown only when it really is text (see `wallet_core::messages::Post::text`).
pub fn draw_board(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    use super::super::eco::BoardRow;
    // A network without a board has nothing to read or write, and no amount of retrying will
    // change that: say what a board is and how this network comes to have one.
    if app.net().is_some_and(|n| n.ecosystem.messages.is_none()) {
        let block = panel(t, "board", true);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let lines = vec![
            Line::from(Span::styled(format!("{}  No message board on {}.", t.icon(Icon::Info), app.dash.network_name), t.strong_style())),
            Line::from(""),
            Line::from(Span::styled(
                "A board is a contract with no owner. Anyone can deploy one, and this wallet reads whichever board the \
                 network is pointed at — so a board exists for everyone pointed at the same address.",
                t.dim_style(),
            )),
            Line::from(""),
            Line::from(Span::styled("Deploy one with the quai-messages project, then point this network at it:", t.dim_style())),
            Line::from(""),
            Line::from(Span::styled("    quai-terminal network add … --messages <address>", t.text_style())),
        ];
        let width = inner.width.saturating_sub(4).min(74);
        let rect = Rect::new(inner.x + 2, inner.y + inner.height.saturating_sub(9) / 2, width, inner.height.min(9));
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), rect);
        return;
    }
    let rows = app.board_rows();
    let wide = area.width >= 90;
    let (list_area, main) = if wide {
        let [l, m] = Layout::horizontal([Constraint::Length(26), Constraint::Min(50)]).areas(area);
        (l, m)
    } else {
        let [l, m] = Layout::vertical([Constraint::Length(5), Constraint::Min(8)]).areas(area);
        (l, m)
    };

    // Channels first, then everyone this wallet can write to in private.
    let open = app.board_row();
    let selected = if app.lit_pane() == Some(1) { app.eco.board.channel_selected } else { app.nav.selected };
    let title = match app.eco.board.filter.as_deref() {
        Some(f) => format!("filter: {f}▏"),
        None => "channels · people".to_string(),
    };
    let block = panel(t, &title, app.lit_pane() == Some(0));
    let inner = block.inner(list_area);
    f.render_widget(block, list_area);
    if rows.is_empty() {
        let (text, hints): (&str, &[(&str, &str)]) = match app.eco.board.filter.as_deref() {
            Some(f) if !f.trim().is_empty() => ("Nothing matches that.", &[("esc", "clear the filter")]),
            _ => ("Nothing to read yet.", &[("a", "new channel")]),
        };
        empty(f, inner, t, Icon::Chat, text, hints);
    } else {
        // Public and private are different kinds of thing, so they are not one undifferentiated
        // list: a channel is readable by anyone who looks, a conversation by exactly two people.
        // Each group is announced, and the rows inside it carry their own mark.
        let group_of = |r: &BoardRow| match r {
            BoardRow::Channel(_) => 0u8,
            BoardRow::Messaging | BoardRow::Chat(..) => 1,
            BoardRow::Request(_) => 2,
            BoardRow::Peer(..) => 3,
            BoardRow::Unfollowed(..) => 4,
        };
        let heading = |g: u8| match g {
            0 => "public channels",
            1 => "private",
            2 => "requests",
            3 => "old · read-only",
            _ => "not followed",
        };
        // Display lines: `None` is a heading, `Some(i)` the row at that index in `rows`. A short
        // list has no room to spend on headings, so there the marks carry the distinction alone.
        let grouped = wide;
        let mut lines: Vec<(Option<usize>, u8)> = Vec::with_capacity(rows.len() + 3);
        let mut last = None;
        for (i, r) in rows.iter().enumerate() {
            let g = group_of(r);
            if grouped && last != Some(g) {
                lines.push((None, g));
                last = Some(g);
            }
            lines.push((Some(i), g));
        }
        // Headings take room, so the list scrolls to keep the selected row on screen.
        let selected = selected.min(rows.len() - 1);
        let height = inner.height as usize;
        let at = lines.iter().position(|(i, _)| *i == Some(selected)).unwrap_or(0);
        let offset = if at >= height { at + 1 - height } else { 0 };
        {
            let mut hits = app.input.hits.borrow_mut();
            hits.add(list_area, crate::tui::hit::Target::Pane(0));
            hits.add(main, crate::tui::hit::Target::Pane(1));
            for (k, (index, _)) in lines.iter().enumerate().skip(offset).take(height) {
                if let Some(i) = index {
                    let rect = Rect::new(inner.x, inner.y + (k - offset) as u16, inner.width, 1);
                    hits.add(
                        rect,
                        crate::tui::hit::Target::Row { list: crate::tui::hit::ListId::Screen(Screen::Board, 0), index: *i, key: None },
                    );
                }
            }
        }
        let table: Vec<Row> = lines
            .iter()
            .skip(offset)
            .take(height)
            .map(|(index, group)| {
                let Some(i) = index else {
                    return Row::new(vec![
                        Cell::from(Span::styled(if (1..=3).contains(group) { "◉" } else { "#" }, t.dim_style())),
                        Cell::from(Span::styled(truncate(heading(*group), 15), t.dim_style())),
                        Cell::from(""),
                    ]);
                };
                let (i, r) = (*i, &rows[*i]);
                let (label, count, sealed) = match r {
                    BoardRow::Channel(name) => {
                        // Unread wins the column: how many are waiting matters more than how
                        // many there are.
                        let unread = app.board_unread(name);
                        let n = if unread > 0 {
                            format!("{unread} new")
                        } else {
                            match app.eco.board.posts.get(name).and_then(|r| r.shown()) {
                                Some(Ok(p)) => p.len().to_string(),
                                Some(Err(_)) => t.icon(Icon::Danger).into(),
                                None => String::new(),
                            }
                        };
                        (format!("#{}", truncate(name, 15)), n, false)
                    }
                    // Dimmed and counted: somewhere to look, not somewhere you keep.
                    BoardRow::Unfollowed(name, messages) => (format!("#{}", truncate(name, 15)), messages.to_string(), false),
                    BoardRow::Peer(code, name) => {
                        let n = match app.eco.board.dms.get(code).and_then(|r| r.shown()) {
                            Some(Ok(l)) => l.len().to_string(),
                            Some(Err(_)) => t.icon(Icon::Danger).into(),
                            None => String::new(),
                        };
                        (truncate(name.as_deref().unwrap_or(&wallet_core::session::short_code(code)), 15), n, true)
                    }
                    BoardRow::Messaging => {
                        use wallet_core::messaging::service::KeyNeed;
                        match app.messaging().map(|v| v.status.need) {
                            Some(KeyNeed::NotSetUp) => ("set up messages".to_string(), String::new(), true),
                            Some(KeyNeed::Publish | KeyNeed::NoKeys) => ("your account".to_string(), t.icon(Icon::Danger).into(), true),
                            _ => ("your account".to_string(), String::new(), true),
                        }
                    }
                    BoardRow::Chat(address, name) => {
                        let label = name.clone().unwrap_or_else(|| short_address(address));
                        (truncate(&label, 15), private_count(app, t, address), true)
                    }
                    BoardRow::Request(address) => (truncate(&short_address(address), 15), private_count(app, t, address), true),
                };
                let followed = !matches!(r, BoardRow::Unfollowed(..));
                // Pinned beside every screen, and notifying: said after the name.
                let (target, _) = App::chat_target(r);
                let pinned = app.eco.board.pin.as_deref() == Some(target.as_str());
                let notifying = app.eco.board.subs.contains(&target);
                let label = format!(
                    "{label}{}{}",
                    if notifying { format!(" {}", t.icon(Icon::Bell)) } else { String::new() },
                    if pinned { " ▸" } else { "" }
                );
                let waiting = matches!(r, BoardRow::Channel(name) if app.board_unread(name) > 0);
                let row = Row::new(vec![
                    // A filled circle marks the rows nobody else can read.
                    Cell::from(Span::styled(if sealed { "◉" } else { " " }, Style::default().fg(t.ok))),
                    Cell::from(Span::styled(label, if followed { t.strong_style() } else { t.dim_style() })),
                    Cell::from(
                        Line::from(Span::styled(count, if waiting { t.strong_style().fg(t.link) } else { t.dim_style() }))
                            .alignment(Alignment::Right),
                    ),
                ]);
                if i == selected { row.style(t.selected()) } else { row }
            })
            .collect();
        f.render_widget(Table::new(table, [Constraint::Length(1), Constraint::Min(8), Constraint::Length(4)]).column_spacing(1), inner);
    }

    let Some(open) = open else {
        let block = panel(t, "messages", app.lit_pane() == Some(1));
        let inner = block.inner(main);
        f.render_widget(block, main);
        return empty(f, inner, t, Icon::Chat, "Follow a channel to read it.", &[("a", "follow a channel")]);
    };
    match open {
        BoardRow::Channel(channel) | BoardRow::Unfollowed(channel, _) => draw_board_channel(f, app, t, main, &channel),
        BoardRow::Peer(code, name) => draw_board_conversation(f, app, t, main, &code, name.as_deref()),
        BoardRow::Messaging => draw_messaging_account(f, app, t, main),
        BoardRow::Chat(address, _) => draw_private_conversation(f, app, t, main, &address, false),
        BoardRow::Request(address) => draw_private_conversation(f, app, t, main, &address, true),
    }
}

/// A private conversation's count in the list: an identity change first, then what is unread,
/// then how many there are.
fn private_count(app: &App, t: &Theme, address: &str) -> String {
    match app.private_conversation(address) {
        Some(c) if c.identity_changed => t.icon(Icon::Danger).into(),
        Some(c) if c.unread > 0 => format!("{} new", c.unread),
        Some(c) => c.messages.to_string(),
        None => String::new(),
    }
}

/// The messaging account: where messages go from, its balance, fingerprint and this week's key,
/// and the accounts to choose it from (never the main one).
pub(crate) fn draw_messaging_account(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    use wallet_core::messaging::service::KeyNeed;
    let block = panel(t, "messaging account", app.lit_pane() == Some(1));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if app.lock.locked {
        return empty(f, inner, t, Icon::Lock, "Unlock to set up private messages.", &[]);
    }
    let Some(view) = app.messaging() else {
        return empty_state(f, inner, t, spinner(), "Reading…", &[]);
    };
    let status = &view.status;
    let current = status.account.clone().filter(|_| status.need != KeyNeed::NotSetUp);
    let mut lines: Vec<Line> = Vec::new();
    let label = |k: &str| Span::styled(format!("{k:<13}"), t.dim_style());
    match &current {
        None => {
            lines.push(Line::from(Span::styled(
                format!("{}  Choose the account your messages go from", t.icon(Icon::Chat)),
                t.strong_style(),
            )));
            lines.push(Line::from(""));
            for p in [
                "It is never your main account: whatever an account posts is tied to what it holds. You fund it, and that transfer is public.",
                "Each message is encrypted to one person. On chain, anyone sees that this account sent something, when and roughly how big, not who it was for.",
                "Its keys stay on this computer and are never backed up: a restore starts a new identity with no history.",
            ] {
                // Wrapped here, so the list below starts exactly where the text ends.
                for (i, part) in crate::tui::ui::textwrap(p, inner.width.saturating_sub(4) as usize).into_iter().enumerate() {
                    lines.push(Line::from(Span::styled(format!("{}{part}", if i == 0 { "•  " } else { "   " }), t.dim_style())));
                }
            }
        }
        Some(account) => {
            let entry = app.dash.accounts.iter().find(|a| a.address.eq_ignore_ascii_case(account));
            let name = entry.map_or_else(|| short_address(account), |a| format!("{} · {}", a.label, short_address(&a.address)));
            lines.push(Line::from(vec![label("account"), Span::styled(name, t.strong_style())]));
            let balance = entry.map(|a| {
                format!("{} QUAI", wallet_core::amount::group_thousands(&wallet_core::amount::format_amount_short(a.balance, 18, 4)))
            });
            let empty_account = entry.is_some_and(|a| a.balance.is_zero());
            lines.push(Line::from(vec![
                label("balance"),
                Span::styled(
                    balance.unwrap_or_else(|| "—".into()),
                    if empty_account { Style::default().fg(t.danger) } else { t.text_style() },
                ),
                Span::styled(if empty_account { "   it pays for every message: F funds it" } else { "" }, t.dim_style()),
            ]));
            if let Some(fp) = &status.fingerprint {
                lines.push(Line::from(vec![label("fingerprint"), Span::styled(fp.clone(), t.text_style())]));
            }
            let key = match status.need {
                KeyNeed::Ready => Span::styled("published", t.text_style()),
                KeyNeed::Publishing => Span::styled("on its way", t.text_style()),
                KeyNeed::Publish => Span::styled("not published yet: K", Style::default().fg(t.danger)),
                KeyNeed::NoKeys => {
                    Span::styled("none on this computer: pick an account below to start again", Style::default().fg(t.danger))
                }
                KeyNeed::NotSetUp => Span::raw(""),
            };
            lines.push(Line::from(vec![label("this week"), key]));
        }
    }
    lines.push(Line::from(""));
    if current.is_some() {
        lines.push(hint_line(t, &[("F", "fund it"), ("K", "publish this week's key"), ("m", "new message")]));
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(if current.is_some() { "move messaging to" } else { "messages go from" }, t.dim_style())));
    let head = lines.len() as u16;
    let [top, list] = Layout::vertical([Constraint::Length(head), Constraint::Min(1)]).areas(inner);
    f.render_widget(Paragraph::new(lines), top);
    let cursor = (app.lit_pane() == Some(1)).then_some(app.nav.selected);
    let rows: Vec<Line> = app
        .messaging_choices()
        .iter()
        .enumerate()
        .map(|(i, (address, text))| {
            let in_use = current.as_deref().is_some_and(|c| address.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(c)));
            let line = Line::from(vec![
                Span::styled(if in_use { format!(" {} ", t.icon(Icon::Ok)) } else { "   ".into() }, Style::default().fg(t.ok)),
                Span::styled(text.clone(), if in_use { t.strong_style() } else { t.text_style() }),
            ]);
            if cursor == Some(i) { line.style(t.selected()) } else { line }
        })
        .collect();
    f.render_widget(Paragraph::new(rows), list);
    if cursor.is_none() {
        let hint = Rect { y: list.y + list.height.saturating_sub(1), height: 1, ..list };
        f.render_widget(Paragraph::new(hint_line(t, &[("enter", "choose from this list")])), hint);
    }
}

/// One private conversation, or a request: what they said, oldest first, under whatever needs
/// saying first (an identity change, a request waiting, a key to publish).
pub(crate) fn draw_private_conversation(f: &mut Frame, app: &App, t: &Theme, area: Rect, address: &str, request: bool) {
    use wallet_core::messaging::service::KeyNeed;
    let who = app.chat_label(&format!("msg:{address}"));
    let loading = app.eco.board.msg.loading();
    let title = format!("{who} · {}{}", if request { "request" } else { "private" }, if loading { " · reading…" } else { "" });
    let block = panel(t, &title, app.lit_pane() == Some(1));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if app.lock.locked {
        return empty(f, inner, t, Icon::Lock, "Unlock to read this conversation.", &[]);
    }
    let convo = app.private_conversation(address);
    let mut banner: Vec<Line> = Vec::new();
    if request {
        banner.push(hint_line(t, &[("a", "accept"), ("B", "block")]));
        banner.push(Line::from(Span::styled("Wrote to you first. Nothing notifies until you accept them.", t.dim_style())));
    } else if convo.is_some_and(|c| c.identity_changed) {
        banner.push(Line::from(Span::styled(
            format!("{}  Their identity key changed. Compare fingerprints (v), then accept it (T), before writing.", t.icon(Icon::Danger)),
            Style::default().fg(t.danger),
        )));
    } else if convo.is_some_and(|c| !c.verified) {
        banner.push(Line::from(Span::styled("Not verified yet: compare fingerprints with them (v).", t.dim_style())));
    }
    if app.messaging().is_some_and(|v| v.status.need == KeyNeed::Publish) {
        banner.push(Line::from(Span::styled("This week's messaging key is not published yet (K).", t.dim_style())));
    }
    let [top, body] = Layout::vertical([Constraint::Length(banner.len() as u16), Constraint::Min(1)]).areas(inner);
    f.render_widget(Paragraph::new(banner), top);
    let lines = match app.eco.board.msg_lines.get(address) {
        Some(Err(e)) => return empty_state(f, body, t, t.icon(Icon::Danger), &app::friendly_error(e), &[("R", "retry")]),
        None => return empty_state(f, body, t, spinner(), "Opening the conversation…", &[]),
        Some(Ok(_)) => app.board_private_lines(),
    };
    if lines.is_empty() {
        return empty(f, body, t, Icon::Chat, "Nothing between you yet.", &[("p", "write the first")]);
    }
    let rows: Vec<(u64, bool, String, Option<String>)> = lines
        .iter()
        .map(|l| {
            let mut text = l.text.clone();
            if l.unverified {
                text = format!("[unverified identity] {text}");
            }
            if l.outgoing && l.status != "sent" {
                text = format!("{text}   · {}", l.status);
            }
            (l.at, l.outgoing, who.clone(), Some(text))
        })
        .collect();
    draw_message_rows(f, app, t, body, &rows);
}

/// The pinned chat, docked beside whatever screen is open: the latest messages, newest at the
/// bottom, and the key that writes to it.
pub fn draw_chat_dock(f: &mut Frame, app: &App, t: &Theme, area: Rect, pin: &str) {
    let label = app.chat_label(pin);
    let notifying = app.eco.board.subs.iter().any(|s| s == pin);
    let title = format!("{label}{}", if notifying { format!(" · {}", t.icon(Icon::Bell)) } else { String::new() });
    let block = panel(t, &title, app.dock.focus);
    let inner = block.inner(area);
    f.render_widget(block, area);
    // The message box on the last row: what is being written, or how to start.
    let [inner, composer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    let room = composer.width.saturating_sub(3) as usize;
    let draft = &app.dock.draft;
    // The end of a long draft, where the cursor is.
    let shown: String = {
        let chars: Vec<char> = draft.chars().collect();
        chars[chars.len().saturating_sub(room.saturating_sub(1))..].iter().collect()
    };
    let composer_line = if app.dock.focus {
        Line::from(vec![Span::styled("› ", t.strong_style().fg(t.focus)), Span::styled(format!("{shown}▏"), t.strong_style())])
    } else if !draft.is_empty() {
        Line::from(vec![Span::styled("› ", t.dim_style()), Span::styled(shown, t.dim_style())])
    } else {
        Line::from(Span::styled("› tab or ` to write", t.dim_style()))
    };
    f.render_widget(Paragraph::new(composer_line).style(Style::default().bg(t.raised)), composer);
    // (time, mine, who, text), oldest first.
    let private = pin.strip_prefix("msg:");
    let lines: Vec<(u64, bool, String, String)> = match (private, pin.strip_prefix("dm:")) {
        (Some(address), _) => {
            if app.lock.locked {
                return empty(f, inner, t, Icon::Lock, "Unlock to read.", &[]);
            }
            match app.eco.board.msg_lines.get(address) {
                Some(Ok(l)) => l.iter().map(|l| (l.at, l.outgoing, label.clone(), l.text.clone())).collect(),
                Some(Err(e)) => return empty_state(f, inner, t, t.icon(Icon::Danger), &app::friendly_error(e), &[]),
                None => return empty_state(f, inner, t, spinner(), "Opening…", &[]),
            }
        }
        (None, Some(code)) => {
            if app.lock.locked {
                return empty(f, inner, t, Icon::Lock, "Unlock to read.", &[]);
            }
            match app.eco.board.dms.get(code).and_then(|r| r.shown()) {
                Some(Ok(l)) => l
                    .iter()
                    .map(|l| {
                        let who = app.contact_name_for(&l.from).unwrap_or_else(|| short_address(&l.from));
                        (l.at, l.mine, who, l.text.clone().unwrap_or_else(|| "<cannot read>".into()))
                    })
                    .collect(),
                Some(Err(e)) => return empty_state(f, inner, t, t.icon(Icon::Danger), &app::friendly_error(e), &[]),
                None => return empty_state(f, inner, t, spinner(), "Opening…", &[]),
            }
        }
        (None, None) => {
            let mine: Vec<String> = app.dash.accounts.iter().map(|a| a.address.to_lowercase()).collect();
            match app.eco.board.posts.get(pin.trim_start_matches('#')).and_then(|r| r.shown()) {
                Some(Ok(posts)) => posts
                    .iter()
                    .rev()
                    .map(|p| {
                        let who = app.contact_name_for(&p.from).unwrap_or_else(|| short_address(&p.from));
                        (p.at, mine.contains(&p.from.to_lowercase()), who, p.text().unwrap_or_else(|| "<sealed>".into()))
                    })
                    .collect(),
                Some(Err(e)) => return empty_state(f, inner, t, t.icon(Icon::Danger), &app::friendly_error(e), &[]),
                None => return empty_state(f, inner, t, spinner(), "Reading…", &[]),
            }
        }
    };
    if lines.is_empty() {
        return empty(f, inner, t, Icon::Chat, "Quiet so far.", &[]);
    }
    // Every message whole: the first line after its age and sender, the rest wrapped beneath,
    // indented under the sender. Filled from the newest up, so the latest is always in view.
    let width = inner.width as usize;
    let height = inner.height as usize;
    let mut rows: Vec<Line> = Vec::new();
    for (at, mine, who, text) in lines.iter().rev() {
        let who = if *mine { "you".to_string() } else { truncate(who, 12) };
        let lead = 5 + unicode_width::UnicodeWidthStr::width(who.as_str()) + 2;
        let body_style = if *mine { t.strong_style() } else { t.text_style() };
        let wrapped = wrap_words(text, width.saturating_sub(lead).max(4), width.saturating_sub(CONTINUE).max(4));
        let mut message: Vec<Line> = Vec::with_capacity(wrapped.len());
        for (i, part) in wrapped.into_iter().enumerate() {
            message.push(if i == 0 {
                Line::from(vec![
                    Span::styled(format!("{:>4} ", ago_short(wallet_core::registry::now().saturating_sub(*at))), t.dim_style()),
                    Span::styled(who.clone(), if *mine { t.strong_style() } else { t.strong_style().fg(t.link) }),
                    Span::styled("  ", t.dim_style()),
                    Span::styled(part, body_style),
                ])
            } else {
                Line::from(vec![Span::raw(" ".repeat(CONTINUE)), Span::styled(part, body_style)])
            });
        }
        // Newest last: this message goes above what is already stacked.
        message.append(&mut rows);
        rows = message;
        if rows.len() >= height {
            break;
        }
    }
    // A message taller than what is left shows its end, where the newest words are; a quiet
    // chat sits at the bottom, as a conversation does.
    let skip = rows.len().saturating_sub(height);
    let mut shown = rows.split_off(skip);
    let mut padded = vec![Line::from(""); height.saturating_sub(shown.len())];
    padded.append(&mut shown);
    f.render_widget(Paragraph::new(padded), inner);
}

/// Where a dock message's wrapped lines start: under the sender, past the age.
pub(crate) const CONTINUE: usize = 5;

/// Words into lines of at most `first` columns, then `rest`. A word longer than a line (an
/// address, a link) is split across lines rather than cut.
pub(crate) fn wrap_words(text: &str, first: usize, rest: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut used = 0usize;
    let limit = |n: usize| if n == 0 { first } else { rest };
    for word in text.split_whitespace() {
        let w = unicode_width::UnicodeWidthStr::width(word);
        let room = limit(out.len());
        let gap = usize::from(!line.is_empty());
        if used + gap + w <= room {
            if gap == 1 {
                line.push(' ');
            }
            line.push_str(word);
            used += gap + w;
            continue;
        }
        if !line.is_empty() && w <= limit(out.len() + 1) {
            out.push(std::mem::take(&mut line));
            line.push_str(word);
            used = w;
            continue;
        }
        // Too long for any line: fill this one, then carry on across the next.
        if !line.is_empty() {
            if used + 1 < limit(out.len()) {
                line.push(' ');
                used += 1;
            } else {
                out.push(std::mem::take(&mut line));
                used = 0;
            }
        }
        for c in word.chars() {
            let cw = c.width().unwrap_or(0);
            if used > 0 && used + cw > limit(out.len()) {
                out.push(std::mem::take(&mut line));
                used = 0;
            }
            line.push(c);
            used += cw;
        }
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

/// One public channel, oldest first.
pub(crate) fn draw_board_channel(f: &mut Frame, app: &App, t: &Theme, area: Rect, channel: &str) {
    let loading = app.eco.board.posts.get(channel).is_some_and(|r| r.loading());
    let title = format!("#{channel}{}", if loading { " · reading…" } else { "" });
    let block = panel(t, &title, app.lit_pane() == Some(1));
    let inner = block.inner(area);
    f.render_widget(block, area);
    match app.eco.board.posts.get(channel).and_then(|r| r.shown()) {
        Some(Err(e)) => return empty_state(f, inner, t, t.icon(Icon::Danger), &app::friendly_error(e), &[("R", "retry")]),
        None if loading => return empty_state(f, inner, t, spinner(), "Reading the board…", &[]),
        None => return empty(f, inner, t, Icon::Chat, "Nothing read yet.", &[("R", "read")]),
        Some(Ok(_)) => {}
    }
    let posts = app.board_posts();
    if posts.is_empty() {
        return empty(f, inner, t, Icon::Chat, "No messages in this channel yet.", &[("enter", "write the first")]);
    }
    let mine: Vec<String> = app.dash.accounts.iter().map(|a| a.address.to_lowercase()).collect();
    let lines: Vec<(u64, bool, String, Option<String>)> = posts
        .iter()
        .map(|p| {
            let body = match p.text() {
                Some(text) => Some(text),
                // A sealed or unreadable body is described, never guessed at.
                None if p.kind == wallet_core::messages::KIND_TEXT => None,
                None => Some(format!("<sealed, {} bytes>", p.body.len())),
            };
            // A post from someone in your contacts reads as their name, not their address.
            let who = app.contact_name_for(&p.from).unwrap_or_else(|| short_address(&p.from));
            (p.at, mine.contains(&p.from.to_lowercase()), who, body)
        })
        .collect();
    draw_message_rows(f, app, t, inner, &lines);
}

/// One sealed conversation. Reading it needs the wallet unlocked, because the key is derived
/// from its payment account.
pub(crate) fn draw_board_conversation(f: &mut Frame, app: &App, t: &Theme, area: Rect, code: &str, name: Option<&str>) {
    // The code is in the title even when the peer has a name: a conversation is with a payment
    // code, not with a contact, and a name over the wrong code is exactly how two people end up
    // in two different conversations, each seeing only what they sent.
    let short = wallet_core::session::short_code(code);
    let who = match name {
        Some(name) => format!("{name} · {short}"),
        None => short,
    };
    let loading = app.eco.board.dms.get(code).is_some_and(|r| r.loading());
    let title = format!("{who} · sealed{}", if loading { " · reading…" } else { "" });
    let block = panel(t, &title, app.lit_pane() == Some(1));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if app.lock.locked {
        return empty(f, inner, t, Icon::Lock, "Unlock to read this conversation.", &[]);
    }
    match app.eco.board.dms.get(code).and_then(|r| r.shown()) {
        Some(Err(e)) => return empty_state(f, inner, t, t.icon(Icon::Danger), &app::friendly_error(e), &[("R", "retry")]),
        None if loading => return empty_state(f, inner, t, spinner(), "Opening the conversation…", &[]),
        None => return empty(f, inner, t, Icon::Chat, "Nothing read yet.", &[("R", "read")]),
        Some(Ok(_)) => {}
    }
    let lines = app.board_dm_lines();
    if lines.is_empty() {
        return empty(f, inner, t, Icon::Chat, "Nothing between you yet.", &[("enter", "write the first"), ("space c", "name them")]);
    }
    let rows: Vec<(u64, bool, String, Option<String>)> = lines
        .iter()
        .map(|l| {
            let who = app.contact_name_for(&l.from).unwrap_or_else(|| short_address(&l.from));
            // Posted from an account not on record for this contact. The body opening does not prove
            // who posted it, so it is pointed out; `c` records the account if the user knows it.
            let text = match (&l.text, l.new_address) {
                (Some(text), true) => Some(format!("{text}   · from an unrecorded address {}", short_address(&l.from))),
                _ => l.text.clone(),
            };
            (l.at, l.mine, who, text)
        })
        .collect();
    draw_message_rows(f, app, t, inner, &rows);
}

/// Messages as a conversation: newest at the bottom, your own marked, and a body that would not
/// open described rather than guessed at.
pub(crate) fn draw_message_rows(f: &mut Frame, app: &App, t: &Theme, area: Rect, lines: &[(u64, bool, String, Option<String>)]) {
    let height = area.height as usize;
    let cursor = (app.lit_pane() == Some(1)).then(|| app.nav.selected.min(lines.len().saturating_sub(1)));
    let offset = cursor.map_or(lines.len().saturating_sub(height), |c| if c >= height { c + 1 - height } else { 0 });
    let rows: Vec<Row> = lines
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(i, (at, mine, from, body))| {
            let who = if *mine { Span::styled("you", t.strong_style()) } else { Span::styled(from.clone(), t.dim_style()) };
            let text = match body {
                Some(text) => Span::styled(text.clone(), if *mine { t.strong_style() } else { t.text_style() }),
                None => Span::styled("<cannot read this>", t.dim_style()),
            };
            let row = Row::new(vec![
                Cell::from(Line::from(Span::styled(flow_age(*at), t.dim_style())).alignment(Alignment::Right)),
                Cell::from(Line::from(who).alignment(Alignment::Right)),
                Cell::from(Line::from(text)),
            ]);
            if cursor == Some(i) { row.style(t.selected()) } else { row }
        })
        .collect();
    f.render_widget(Table::new(rows, [Constraint::Length(5), Constraint::Length(12), Constraint::Min(20)]).column_spacing(1), area);
}
