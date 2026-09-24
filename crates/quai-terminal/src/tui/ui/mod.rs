//! Rendering. Widgets use semantic theme roles only; color never carries meaning alone.

use super::app::{self, ACTIONS, App, FieldKind, Modal, OnboardKind, Onboarding, Picker, Screen};
use super::hit::{HeaderPart, Target};
use super::icons::Icon;
use super::terminal::Tier;
use super::theme::Theme;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Clear, Padding, Paragraph, Row, Sparkline, Table, Wrap};
use wallet_core::amount;
use wallet_core::appdb::{Activity, OpStatus, Operation};
use wallet_core::config::Motion;
use wallet_core::sdk::U256;
use wallet_core::session::{short_address, short_code};
use wallet_core::track::{describe, human_duration};

pub(crate) mod layout;
mod lock;
mod modals;
mod screens;
pub(crate) use layout::{Breakpoint, SHORT_ROWS, with_inspector};
pub(crate) use lock::*;
pub use modals::*;
pub(crate) use screens::*;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// How long a spinner glyph shows.
const SPINNER_MS: u128 = 80;

fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

std::thread_local! {
    /// Whether the frame being drawn shows a spinner (any `spinner()` call sets it).
    static SPUN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The frames drawn with `modal_frame` this frame: pictures inside one stay placed while a
    /// modal is open, and pictures behind the glass are taken down (`images`).
    static FRAMED: std::cell::RefCell<Vec<Rect>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Motion is Off: spinners stand still (`◌`) and ask for no frames.
    static STILL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether `rect` lies inside a frame drawn this frame (a modal, a sheet).
pub(crate) fn inside_a_frame(rect: Rect) -> bool {
    FRAMED.with(|f| {
        f.borrow().iter().any(|m| m.contains(rect.as_position()) && m.contains(Position::new(rect.right() - 1, rect.bottom() - 1)))
    })
}

pub(crate) fn spinner() -> &'static str {
    // Motion Off means off: a still mark, and no redraws at the spinner's pace.
    if STILL.with(|s| s.get()) {
        return super::icons::Icon::InFlight.glyph(super::icons::Set::Unicode);
    }
    SPUN.with(|s| s.set(true));
    SPINNER[spinner_step() as usize % SPINNER.len()]
}

/// Which spinner glyph is showing now; a frame is due when it changes.
pub(crate) fn spinner_step() -> u128 {
    now_ms() / SPINNER_MS
}

pub(crate) fn until_next_spinner_step() -> std::time::Duration {
    std::time::Duration::from_millis((SPINNER_MS - now_ms() % SPINNER_MS) as u64)
}

/// How long the header's block glyph pulses after a new block.
pub(crate) const BEAT_PULSE: std::time::Duration = std::time::Duration::from_millis(700);

pub(crate) fn q(v: U256) -> String {
    amount::group_thousands(&amount::format_amount_short(v, 18, 4))
}

pub(crate) fn qi(v: U256) -> String {
    amount::group_thousands(&amount::qi(v))
}

/// How long ago a unix time was: `now`, `12m`, `2h 30m`, `3d 4h`. A time that was never known
/// (zero) is `—`, never "56 years ago".
pub(crate) fn ago(secs: u64) -> String {
    if secs == 0 {
        return "—".into();
    }
    let age = wallet_core::registry::now().saturating_sub(secs);
    if age < 60 { "now".into() } else { human_duration(age) }
}

/// [`ago`] in at most four cells, for a narrow column: `now`, `45s`, `12m`, `10h`, `3d`, `5w`.
pub(crate) fn ago_short(secs: u64) -> String {
    if secs == 0 {
        return "—".into();
    }
    let age = wallet_core::registry::now().saturating_sub(secs);
    match age {
        0..60 => "now".into(),
        60..3_600 => format!("{}m", age / 60),
        3_600..86_400 => format!("{}h", age / 3_600),
        86_400..1_209_600 => format!("{}d", age / 86_400),
        _ => format!("{}w", age / 604_800),
    }
}

/// Plain-language outcome of an operation kind, shown in every review.
fn review_story(kind: &str) -> Vec<String> {
    let locks = format!("the time locks under {}", Screen::Accounts.place());
    let steps: Vec<String> = match kind {
        "convert_quai_to_qi" => vec![
            "signed and broadcast; included within a few blocks".into(),
            "if the block's shared discount exceeds your slippage, it refunds (the fee is spent)".into(),
            format!("otherwise Qi arrives time-locked and counts down to spendable in {locks}"),
        ],
        "convert_qi_to_quai" => {
            vec![
                "signed and broadcast; included within a few blocks".into(),
                format!("QUAI arrives time-locked in the account; it counts down in {locks}"),
            ]
        }
        "send_qi" => vec![
            "each output lands on a fresh one-time address".into(),
            "the recipient finds it with their payment code (mailbox or channel scan)".into(),
        ],
        "wrap_qi" => vec![format!("Qi moves into the wrapper; once settled, claim WQI in {}", Screen::Wrap.place())],
        "nft_list" | "nft_reprice" => vec![
            "a Zora ask goes live on-chain; Bazarr shows it within a minute".into(),
            "the item stays in your wallet until someone buys it at this price".into(),
            "when it sells, the proceeds arrive and the wallet notifies you (NFT sold)".into(),
        ],
        "nft_unlist" => vec!["the ask is removed on-chain; nobody can buy the item at the old price".into()],
        "unwrap_wqi" => vec!["WQI is burned; Qi returns after the protocol lock".into()],
        "fill_gap" => vec!["uses the unused nonce; transactions queued behind it can then be mined".into()],
        "aggregate_qi" | "sweep_qi" => {
            vec!["coins merge into fewer outputs you own; aggregation must be first in a block, so it may wait".into()]
        }
        _ => vec![format!("signed and broadcast; included within a few blocks and tracked in {}", Screen::Activity.place())],
    };
    steps
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>()) }
}

/// Hairline panel with one cell of horizontal padding; only the focused panel gets the accent.
pub(crate) fn panel<'a>(t: &Theme, title: &str, focused: bool) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(t.border(focused))
        .padding(Padding::horizontal(1))
        .title(if focused {
            Span::styled(format!(" ▸ {title} "), t.strong_style().fg(t.focus))
        } else {
            Span::styled(format!(" {title} "), t.dim_style())
        })
}

pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2)).max(1);
    let h = height.min(area.height.saturating_sub(2)).max(1);
    Rect::new(area.x + (area.width.saturating_sub(w)) / 2, area.y + (area.height.saturating_sub(h)) / 2, w, h)
}

/// Break prose into lines of at most `width` columns, on spaces.
///
/// `Paragraph`'s own `Wrap` already does this when rendering; this exists so a modal can size
/// itself to prose before it renders, and so a wrapped block can be styled line by line.
pub(crate) fn textwrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let needed = if line.is_empty() { word.chars().count() } else { line.chars().count() + 1 + word.chars().count() };
        if needed > width && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() { vec![String::new()] } else { lines }
}

pub(crate) fn darken(c: Color, amount: f32) -> Color {
    match c {
        Color::Rgb(r, g, b) => {
            let f = |v: u8| (f32::from(v) * (1.0 - amount)) as u8;
            Color::Rgb(f(r), f(g), f(b))
        }
        other => other,
    }
}

/// Raised modal surface with a soft one-cell shadow. Returns the inner area.
pub(crate) fn modal_frame(f: &mut Frame, rect: Rect, t: &Theme, title: &str) -> Rect {
    FRAMED.with(|m| m.borrow_mut().push(rect));
    let area = f.area();
    // A soft shadow: heavy on dark themes, a light tint on light ones.
    let shadow = t.shadow;
    let buf = f.buffer_mut();
    for y in rect.y + 1..=(rect.y + rect.height).min(area.bottom().saturating_sub(1)) {
        let x = rect.x + rect.width;
        if x < area.right()
            && let Some(cell) = buf.cell_mut((x, y))
        {
            cell.set_bg(shadow);
        }
    }
    for x in rect.x + 1..=(rect.x + rect.width).min(area.right().saturating_sub(1)) {
        let y = rect.y + rect.height;
        if y < area.bottom()
            && let Some(cell) = buf.cell_mut((x, y))
        {
            cell.set_bg(shadow);
        }
    }
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(t.focus).bg(t.raised))
        .style(Style::default().bg(t.raised).fg(t.text))
        .padding(Padding::new(2, 2, 1, 0))
        .title(Span::styled(format!(" {title} "), t.strong_style().fg(t.focus).bg(t.raised)));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    inner
}

pub fn status_glyph(t: &Theme, s: OpStatus) -> &'static str {
    match s {
        OpStatus::Confirmed | OpStatus::Settled => t.icon(Icon::Ok),
        OpStatus::Failed => t.icon(Icon::Danger),
        OpStatus::Refunded => "↩",
        OpStatus::Unknown => "?",
        OpStatus::Replaced => "»",
        OpStatus::Cancelled => "–",
        OpStatus::Locked => t.icon(Icon::Locked),
        _ => t.icon(Icon::InFlight),
    }
}

pub(crate) fn status_style(t: &Theme, s: OpStatus) -> Style {
    let c = match s {
        OpStatus::Confirmed | OpStatus::Settled => t.ok,
        OpStatus::Failed | OpStatus::Refunded => t.danger,
        OpStatus::Unknown => t.attention,
        OpStatus::Cancelled | OpStatus::Replaced => t.dim,
        _ => t.pending,
    };
    Style::default().fg(c)
}

pub(crate) fn kind_icon(t: &Theme, kind: &str) -> &'static str {
    t.icon(match kind {
        k if k.starts_with("convert") => Icon::Convert,
        k if k.contains("swap") => Icon::Swap,
        k if k.contains("unwrap") => Icon::Unwrap,
        k if k.contains("wrap") || k.contains("claim") => Icon::Wrap,
        k if k.contains("approve") || k.contains("revoke") => Icon::Approve,
        k if k.contains("aggregate") || k.contains("sweep") => Icon::Gather,
        k if k.contains("notify") => Icon::Notify,
        _ => Icon::Send,
    })
}

/// An empty state marked with an icon: its picture where Nerd Font icons draw, a quiet `·` where
/// the set has nothing for it.
pub(crate) fn empty(f: &mut Frame, area: Rect, t: &Theme, icon: Icon, text: &str, hints: &[(&str, &str)]) {
    let glyph = match t.icon(icon) {
        "" => t.icon(Icon::Info),
        g => g,
    };
    empty_state(f, area, t, glyph, text, hints)
}

/// Centered empty state: glyph, one line, key hints.
pub(crate) fn empty_state(f: &mut Frame, area: Rect, t: &Theme, glyph: &str, text: &str, hints: &[(&str, &str)]) {
    let mut lines = vec![Line::from(Span::styled(format!("{glyph}  {text}"), t.dim_style()))];
    if !hints.is_empty() {
        let mut spans = Vec::new();
        for (i, (k, v)) in hints.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled("  ·  ", t.dim_style()));
            }
            spans.push(Span::styled(*k, t.strong_style().fg(t.focus)));
            spans.push(Span::styled(format!(" {v}"), t.dim_style()));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(spans));
    }
    let w = lines.iter().map(|line| line.width()).max().unwrap_or(1).min(area.width as usize).max(1) as u16;
    let h = lines.iter().map(|line| line.width().max(1).div_ceil(w as usize)).sum::<usize>().min(area.height as usize) as u16;
    let rect = Rect::new(area.x + area.width.saturating_sub(w) / 2, area.y + area.height.saturating_sub(h) / 2, w.min(area.width), h);
    f.render_widget(Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }), rect);
}

// ---------------------------------------------------------------- big digits

const DIGITS: [[&str; 3]; 10] = [
    ["█▀█", "█ █", "▀▀▀"],
    ["▀█ ", " █ ", "▀▀▀"],
    ["▀▀█", "█▀▀", "▀▀▀"],
    ["▀▀█", " ▀█", "▀▀▀"],
    ["█ █", "▀▀█", "  ▀"],
    ["█▀▀", "▀▀█", "▀▀▀"],
    ["█▀▀", "█▀█", "▀▀▀"],
    ["▀▀█", "  █", "  ▀"],
    ["█▀█", "█▀█", "▀▀▀"],
    ["█▀█", "▀▀█", "▀▀▀"],
];

/// Three-row block digits for the whole part of a grouped number (e.g. "179,071").
pub fn big_digits(text: &str) -> [String; 3] {
    let mut rows = [String::new(), String::new(), String::new()];
    for (i, c) in text.chars().enumerate() {
        if i > 0 {
            for r in &mut rows {
                r.push(' ');
            }
        }
        match c {
            d @ '0'..='9' => {
                let g = DIGITS[d as usize - '0' as usize];
                for (r, part) in rows.iter_mut().zip(g) {
                    r.push_str(part);
                }
            }
            ',' => {
                // A stroke from the baseline down: a lone baseline block would read as a decimal
                // point, and a gap read "1,284" as "1 284".
                rows[0].push(' ');
                rows[1].push(' ');
                rows[2].push('▌');
            }
            _ => {
                for r in &mut rows {
                    r.push(' ');
                }
            }
        }
    }
    rows
}

/// A balance "hero": big whole part, dim fraction and unit, ledger stripe.
/// Whether both balances fit as block digits (they share one size).
pub(crate) fn hero_fits(width: u16, height: u16, values: &[&str]) -> bool {
    height >= 5 && values.iter().all(|v| big_digits(v.split('.').next().unwrap_or(v))[0].chars().count() as u16 + 12 <= width)
}

// ---------------------------------------------------------------- frame

/// A height worth marking: a whole million, or one digit repeated (5,555,555).
pub fn milestone(height: u64) -> bool {
    let digits = height.to_string();
    height >= 1_000_000 && (height.is_multiple_of(1_000_000) || digits.chars().all(|c| digits.starts_with(c)))
}

/// Draw one frame.
pub fn draw(f: &mut Frame, app: &mut App) {
    app.eco.inline_icons.borrow_mut().clear();
    app.hits.borrow_mut().clear();
    app.last_size = (f.area().width, f.area().height);
    // When a different modal appears, note the moment: clicks in the next instant were aimed at
    // what was there before (see `pointer::MODAL_GRACE`).
    let kind = modal_code(&app.modal);
    if kind != app.modal_kind.get() {
        app.modal_kind.set(kind);
        app.modal_since.set((kind != 0).then(std::time::Instant::now));
    }
    app.theme.icons = app.icon_set();
    SPUN.with(|s| s.set(false));
    STILL.with(|s| s.set(app.motion() == Motion::Off));
    FRAMED.with(|m| m.borrow_mut().clear());
    draw_frame(f, app);
    // Any spinner drawn keeps turning until the frame no longer shows one.
    app.spun = SPUN.with(|s| s.get());
    close_clipped_titles(f.buffer_mut());
    let t = app.theme.clone();
    legible_selection(f.buffer_mut(), &t);
    // Icons before the capture: their badge cells are blanked for the bitmaps, and a relit frame
    // must show the same blank cells (the bitmaps themselves stay placed across it).
    super::images::place_inline_icons(app, f.buffer_mut(), &t);
    // Inside tmux, pictures are text the terminal draws them over (see `placeholders`).
    if app.caps.placeholders && super::images::bitmaps(app) {
        super::placeholders::place(app, f.buffer_mut());
    }
    // Hashes and addresses the wallet knows open in the explorer (OSC 8), where they are shown;
    // then what the pointer is over.
    let mut links = super::links::scan(app, f.buffer_mut());
    hover_marks(app, f.buffer_mut(), &t, &mut links);
    // Sized text only where its placeholder survived the whole frame: a toast or a pill drawn
    // over it afterwards wins, and the text is not painted back on top of it.
    {
        use super::term::backend::BIG_TEXT_CELL;
        let buf = &*f.buffer_mut();
        app.big_text.borrow_mut().retain(|b| {
            (b.y..b.y + u16::from(b.scale))
                .all(|y| (b.x..b.x + b.width()).all(|x| buf.cell((x, y)).is_some_and(|c| c.symbol() == BIG_TEXT_CELL)))
        });
    }
    // The composed content, kept for the frames where only the edge light moves (`draw_edges`),
    // while the edges animate at all.
    app.content = app.eco.anim_step.get().is_some().then(|| f.buffer_mut().clone());
    super::edge::paint(app, f.buffer_mut(), &t);
    *app.links_shown.borrow_mut() = links.clone();
    super::term::backend::set_links(links);
    app.focus_at = focus_position(app, f.buffer_mut(), &t);
}

/// Where the keyboard's focus is on screen, for the terminal's own cursor to wait at (hidden),
/// so screen magnifiers and readers that follow the cursor follow the wallet: a text field's
/// caret when one is drawn, otherwise the selected row of the list in front.
fn focus_position(app: &App, buf: &Buffer, t: &Theme) -> Option<(u16, u16)> {
    let area = buf.area;
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if buf.cell((x, y)).is_some_and(|c| c.symbol() == "▏" && c.fg == t.focus) {
                return Some((x, y));
            }
        }
    }
    let list = app.main_list();
    let hits = app.hits.borrow();
    hits.live_regions()
        .iter()
        .find(|(_, target)| matches!(target, Target::Row { list: l, index, .. } if *l == list && *index == app.selected))
        .map(|(r, _)| (r.x, r.y))
}

/// A frame where nothing but the edge light has moved: the last frame's content, relit. A full
/// `draw` costs 1–3 ms on a busy screen; this costs the edge pass. Falls back to `draw` when there
/// is no content of this size to relight.
pub fn draw_edges(f: &mut Frame, app: &mut App) {
    let area = f.area();
    match app.content.as_ref().filter(|b| b.area == area) {
        Some(content) => {
            f.buffer_mut().clone_from(content);
            let t = app.theme.clone();
            super::edge::paint(app, f.buffer_mut(), &t);
        }
        None => draw(f, app),
    }
}

/// A row's jump label: quiet at rest, lit once `'` has armed a jump and it can be pressed.
pub(crate) fn jump_style(app: &App, t: &Theme) -> Style {
    if app.jump_pending.is_some() { t.strong_style().fg(t.focus) } else { t.dim_style() }
}

/// A panel title longer than its panel runs into the corner (`reserves 2┐`). Wherever text
/// touches a top-right corner, the last cell before it becomes `…` and a line segment, so a
/// clipped title says so and the corner stays a corner.
fn close_clipped_titles(buf: &mut ratatui::buffer::Buffer) {
    let area = buf.area;
    for y in area.y..area.bottom() {
        for x in area.x + 2..area.right() {
            if buf[(x, y)].symbol() != "┐" {
                continue;
            }
            let before = buf[(x - 1, y)].symbol();
            if before == " " || before == "─" || "┌┐└┘├┤┬┴┼│".contains(before) {
                continue;
            }
            let corner = buf[(x, y)].style();
            buf[(x - 2, y)].set_symbol("…");
            buf[(x - 1, y)].set_symbol("─").set_style(corner);
        }
    }
}

/// Which modal is open, as a number that changes when the modal does (0: none).
fn modal_code(m: &Modal) -> u8 {
    match m {
        Modal::None => 0,
        Modal::Form(_) => 2,
        Modal::Review(_) => 3,
        Modal::Help => 4,
        Modal::Glossary { .. } => 5,
        Modal::Palette { .. } => 6,
        Modal::Receive { .. } => 7,
        Modal::Secret { .. } => 8,
        Modal::Quote(_) => 9,
        Modal::Result(_) => 10,
        Modal::Notifications => 11,
        Modal::Notice { .. } => 12,
        Modal::Confirm { .. } => 13,
        Modal::Themes(_) => 14,
        Modal::Effects(_) => 15,
        Modal::TokenPicker { .. } => 16,
        Modal::Sheet { .. } => 17,
        Modal::GoTo => 18,
        Modal::Wallets { .. } => 19,
    }
}

/// Colored text on the selection highlight (amounts, addresses, statuses) keeps its theme color
/// only while it stays readable; otherwise it switches to the strong text color.
fn legible_selection(buf: &mut Buffer, t: &Theme) {
    let area = buf.area;
    // The terminal palette's selection is an ANSI grey whose contrast with the row's own colors
    // can't be known; ANSI "bright black" dim text on it is invisible on most palettes. There the
    // selected row takes the terminal's own foreground, which every palette makes readable on its
    // greys; the row's glyphs still carry what its colors did.
    if !matches!(t.selection, Color::Rgb(..)) {
        if t.monochrome {
            return;
        }
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                if let Some(cell) = buf.cell_mut((x, y))
                    && cell.bg == t.selection
                {
                    cell.fg = Color::Reset;
                }
            }
        }
        return;
    }
    let (Color::Rgb(sr, sg, sb), Color::Rgb(tr, tg, tb)) = (t.selection, t.strong) else { return };
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = buf.cell_mut((x, y))
                && cell.bg == t.selection
                && let Color::Rgb(r, g, b) = cell.fg
                && super::theme::contrast((r, g, b), (sr, sg, sb)) < 4.5
            {
                cell.fg = Color::Rgb(tr, tg, tb);
            }
        }
    }
}

/// The smallest terminal anything is drawn in. Below it the frame is one line asking for room,
/// and the keyboard is held back too (see `App::on_key`): nobody acts on a screen they can't see.
pub const MIN_SIZE: (u16, u16) = (60, 18);

pub fn too_small(size: (u16, u16)) -> bool {
    size.0 < MIN_SIZE.0 || size.1 < MIN_SIZE.1
}

fn draw_frame(f: &mut Frame, app: &mut App) {
    let t = app.theme.clone();
    let area = f.area();
    f.render_widget(Block::default().style(t.base()), area);
    app.qr_rect = None;
    app.big_text.borrow_mut().clear();
    app.breakpoint = Breakpoint::of(area.width);
    app.short = area.height < SHORT_ROWS;

    if too_small((area.width, area.height)) {
        let p = Paragraph::new(vec![
            Line::from(Span::styled("◆ quai-terminal", t.strong_style().fg(t.quai))),
            Line::from("Make the terminal at least 60×18."),
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
        f.render_widget(p, centered(area, area.width, 3));
        return;
    }
    if app.onboarding.is_some() {
        draw_onboarding(f, app, &t, area);
        draw_toasts(f, app, &t, area);
        return;
    }
    if app.locked {
        draw_lock(f, app, &t, area);
        draw_modal(f, app, &t, area);
        draw_toasts(f, app, &t, area);
        return;
    }

    let [header, _rule, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(0), Constraint::Min(5), Constraint::Length(1)]).areas(area);
    // Trader: Markets and the swap card side by side, on a terminal wide enough to hold both.
    app.trader = match app.config.layout.as_str() {
        "trader" => body.width >= 140,
        "auto" => body.width >= 200,
        _ => false,
    };
    let (nav, content) = if app.breakpoint > Breakpoint::Compact && app.config.layout != "focus" {
        // The rail widens on a wide terminal, where the columns are there to spare.
        let rail = if app.breakpoint == Breakpoint::Wide { 24 } else { 23 };
        let [n, m] = Layout::horizontal([Constraint::Length(rail), Constraint::Min(40)]).areas(body);
        (Some(n), m)
    } else {
        (None, body)
    };
    draw_header(f, app, &t, header, nav.is_none());
    if let Some(n) = nav {
        draw_nav(f, app, &t, n);
    }
    let section = app.screen.section();
    let main = if section.tab_labels(&app.config.features).len() > 1 && content.height > 8 {
        // Two rows (the labels, and a rule lit under the open one) unless rows are short.
        let rows = if app.short { 1 } else { 2 };
        let [tabs, m] = Layout::vertical([Constraint::Length(rows), Constraint::Min(4)]).areas(content);
        super::views::draw_tabs(f, app, &t, tabs);
        m
    } else {
        content
    };
    // The pinned chat docks beside every screen but the Board (which shows it already): a column
    // on the right when there is width to spare, a strip along the bottom when there is height.
    app.dock_shown = false;
    let main = match app.eco.board.pin.clone() {
        Some(pin) if app.screen != Screen::Board && app.config.features.messaging => {
            if main.width >= 150 {
                let [m, dock] = Layout::horizontal([Constraint::Min(80), Constraint::Length(46)]).areas(main);
                super::views::draw_chat_dock(f, app, &t, dock, &pin);
                app.dock_shown = true;
                m
            } else if main.height >= 30 {
                let [m, dock] = Layout::vertical([Constraint::Min(20), Constraint::Length(9)]).areas(main);
                super::views::draw_chat_dock(f, app, &t, dock, &pin);
                app.dock_shown = true;
                m
            } else {
                main
            }
        }
        _ => main,
    };
    match app.detail.last() {
        Some(d) => super::views::draw_detail(f, app, &t, main, &d.clone()),
        None => match app.screen {
            Screen::Home => super::views::draw_home(f, app, &t, main),
            Screen::Markets | Screen::Swap if app.trader => {
                let [left, right] = Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)]).areas(main);
                super::views::draw_markets(f, app, &t, left);
                super::views::draw_swap(f, app, &t, right);
            }
            Screen::Markets => super::views::draw_markets(f, app, &t, main),
            Screen::Accounts => draw_accounts(f, app, &t, main),
            Screen::Activity => draw_activity(f, app, &t, main),
            Screen::Qi => draw_qi(f, app, &t, main),
            Screen::Contacts => draw_payments(f, app, &t, main, false),
            Screen::Channels => draw_payments(f, app, &t, main, true),
            Screen::Board => super::views::draw_board(f, app, &t, main),
            Screen::Wallets => super::views::draw_wallets(f, app, &t, main),
            Screen::Orders => super::order_ui::draw_screen(f, app, &t, main),
            Screen::Swap => super::views::draw_swap(f, app, &t, main),
            Screen::Pools => super::views::draw_pools(f, app, &t, main),
            Screen::Convert => super::views::draw_convert_card(f, app, &t, main),
            Screen::Wrap => super::views::draw_wrap_card(f, app, &t, main),
            Screen::Launches => super::views::draw_launches(f, app, &t, main),
            Screen::Pnl => super::views::draw_pnl(f, app, &t, main),
            Screen::Collected => super::views::draw_collected(f, app, &t, main),
            Screen::Explore => super::views::draw_explore(f, app, &t, main),
            Screen::Listings => super::views::draw_listings(f, app, &t, main),
            Screen::Network => draw_node(f, app, &t, main),
            Screen::Settings => draw_settings(f, app, &t, main),
            Screen::DataSources => super::views::draw_data_sources(f, app, &t, main),
        },
    }
    let modal_open = !matches!(app.modal, Modal::None);
    // Decorative effects never draw over modals (reviews, secrets, forms).
    if modal_open {
        app.ambient = None;
    }
    if let Some(c) = app.ambient.as_mut() {
        f.render_widget(Clear, main);
        f.render_widget(Block::default().style(t.base()), main);
        // By the clock, not per redraw: keys and data arriving mid-effect don't speed it up.
        if c.advance() {
            c.render(main, f.buffer_mut(), t.base().fg(t.ok));
        } else {
            app.ambient = None;
            // `:poem`: the hash rain gives way to the haiku.
            if let Some(haiku) = app.poem_haiku.take() {
                let args = super::fx::theme_args("decrypt", &t);
                app.ambient =
                    super::fx::Ceremony::with_args("decrypt", &args, &haiku, 100, 30, 300).map(|c| c.at_speed(super::fx::LOCK_SPEED));
            }
        }
    }
    draw_footer(f, app, &t, footer);
    // Before the modal, so its glass dims them with the page.
    draw_scrollbars(f.buffer_mut(), app, &t);
    draw_modal(f, app, &t, area);
    draw_toasts(f, app, &t, area);
}

/// A thumb on the right border of every list that holds more than it shows: where you are, and
/// how much there is. Only over a plain border cell, so nothing else is ever drawn over.
fn draw_scrollbars(buf: &mut Buffer, app: &App, t: &Theme) {
    let lists = app.hits.borrow().lists().to_vec();
    for (area, offset, len) in lists {
        let h = area.height as usize;
        if len <= h || h < 3 {
            continue;
        }
        let border = [area.right(), area.right() + 1]
            .into_iter()
            .find(|x| (area.y..area.bottom()).all(|y| buf.cell((*x, y)).is_some_and(|c| c.symbol() == "│")));
        let Some(x) = border else { continue };
        let size = (h * h / len).max(1);
        let top = (offset * (h - size)).div_ceil((len - h).max(1)).min(h - size);
        for i in 0..size {
            if let Some(c) = buf.cell_mut((x, area.y + (top + i) as u16)) {
                c.set_symbol("┃").set_fg(t.line_strong);
            }
        }
    }
}

/// What the pointer is over, marked without filling anything: a list row gets a faint version
/// of the selection's hot edge, and a clickable label an underline in the accent. Approve &
/// sign never lights under the pointer. Over a shortened hash or address, a tooltip gives it
/// whole, in groups of four (and links it like the original).
fn hover_marks(app: &App, buf: &mut Buffer, t: &Theme, links: &mut Vec<super::links::Link>) {
    use super::hit::ReviewPart;
    let Some((x, y)) = app.pointer.at else { return };
    let region = app.hits.borrow().region_at(x, y).map(|(r, target)| (r, target.clone()));
    match region {
        Some((rect, Target::Row { .. })) => {
            // The panel's edge beside the row: seen whatever the row begins with (an icon's
            // bitmap covers its first cells).
            if let Some(edge) =
                (1..=2u16).filter_map(|d| rect.x.checked_sub(d)).find(|x| buf.cell((*x, rect.y)).is_some_and(|c| c.symbol() == "│"))
                && let Some(c) = buf.cell_mut((edge, rect.y))
            {
                c.set_symbol("┃").set_fg(t.focus);
            }
            if let (Color::Rgb(fr, fg, fb), true) = (t.focus, app.caps.truecolor) {
                for (i, k) in [0.16f32, 0.09, 0.04].into_iter().enumerate() {
                    if let Some(c) = buf.cell_mut((rect.x + i as u16, rect.y))
                        && c.bg != t.selection
                        && let Color::Rgb(r, g, b) = c.bg
                    {
                        c.set_bg(blend((r, g, b), (fr, fg, fb), k));
                    }
                }
            }
        }
        Some((_, Target::Review(ReviewPart::Approve))) => {}
        Some((
            rect,
            Target::Section(_)
            | Target::Tab(_)
            | Target::Key(_)
            | Target::Route(_)
            | Target::Choice { .. }
            | Target::Confirm(_)
            | Target::Button(_),
        )) => {
            for cx in rect.left()..rect.right() {
                if let Some(c) = buf.cell_mut((cx, rect.y))
                    && c.symbol() != " "
                {
                    c.modifier.insert(Modifier::UNDERLINED);
                    c.underline_color = t.focus;
                }
            }
        }
        _ => {}
    }
    if !matches!(app.modal, Modal::None) {
        return;
    }
    let Some(link) = links.iter().find(|l| l.y == y && (l.x..l.end).contains(&x)).cloned() else { return };
    let shown: String = (link.x..link.end).filter_map(|cx| buf.cell((cx, y)).map(|c| c.symbol().to_string())).collect();
    let Some(id) = link.url.rsplit('/').next().filter(|id| id.starts_with("0x")) else { return };
    // The whole id when the screen shortened it, and in every case what a click does: the mouse
    // is the wallet's, so the terminal's own link handling never sees it.
    let mut spans = if shown.contains('…') { super::widgets::address(t, id, t.strong_style()) } else { Vec::new() };
    let hint = if spans.is_empty() { "ctrl+click opens · alt+click copies" } else { "  ctrl+click opens · alt+click copies" };
    spans.push(Span::styled(hint, t.dim_style()));
    let text_w: u16 = spans.iter().map(|s| s.width() as u16).sum();
    let area = buf.area;
    let w = (text_w + 2).min(area.width);
    let ty = if y + 1 < area.bottom().saturating_sub(1) { y + 1 } else { y.saturating_sub(1) };
    let tx = x.saturating_sub(w / 2).min(area.right().saturating_sub(w));
    let rect = Rect::new(tx, ty, w, 1);
    let line = Line::from([vec![Span::raw(" ")], spans, vec![Span::raw(" ")]].concat());
    ratatui::widgets::Widget::render(Paragraph::new(line).style(Style::default().bg(t.raised)), rect, buf);
    // A long hash does not fit grouped; the tooltip still links, from its first cell.
    links.push(super::links::Link { y: ty, x: tx + 1, end: (tx + 1 + text_w).min(rect.right()), url: link.url });
}

fn draw_header(f: &mut Frame, app: &App, t: &Theme, area: Rect, show_screen: bool) {
    let d = &app.dash;
    let sep = || Span::styled(" │ ", t.dim_style());
    let name = app.meta.as_ref().map(|m| m.name.clone()).unwrap_or_default();
    // The wallet's name is the one thing on this bar that says whose money is on screen, so it
    // stands in capitals and the brightest text rather than in the run of grey beside it. (The
    // accent is kept for what has focus.)
    let shown_name = if name.chars().count() <= 22 { name.to_uppercase() } else { name };
    // The bar is segments, each with a priority: when the width runs out, whole segments drop,
    // least needed first, and never mid-word. What wallet this is, what the node says, and
    // whether it can sign stay, whatever the width.
    struct Seg {
        /// 0 never drops; higher drops sooner.
        drop: u8,
        /// Sits against this segment (by position in `segs` as built), with no separator,
        /// while it is still there.
        joined: Option<usize>,
        /// Position in `segs` as built.
        id: usize,
        spans: Vec<Span<'static>>,
        /// (index within `spans`, what a click there does)
        targets: Vec<(usize, Target)>,
    }
    let seg = |drop: u8, spans: Vec<Span<'static>>| Seg { drop, joined: None, id: 0, spans, targets: Vec::new() };
    let mut segs: Vec<Seg> = Vec::new();
    segs.push(Seg {
        targets: vec![(3, Target::Header(HeaderPart::Wallet))],
        ..seg(
            0,
            vec![
                Span::raw(" "),
                super::images::native_span(app, t, "quai"),
                Span::raw(" "),
                Span::styled(shown_name, t.strong_style().add_modifier(Modifier::BOLD)),
            ],
        )
    });
    // The balance rides beside the name, rounded: this is the glance figure, not the ledger.
    // `$` hides it, for a room with other people in it. It is the first thing to go.
    if app.config.balance_in_bar && !app.locked && !d.accounts.is_empty() {
        let quai = d.accounts.iter().fold(wallet_core::sdk::U256::ZERO, |s, a| s.saturating_add(a.balance));
        let shown = wallet_core::amount::group_thousands(&wallet_core::amount::format_amount_short(quai, 18, 2));
        segs.push(Seg { joined: Some(0), ..seg(6, vec![Span::styled(format!("  {shown} QUAI"), t.strong_style())]) });
    }
    if show_screen {
        // Narrow terminals: the nav rail collapses into a section strip. It is the navigation
        // there, so it outlasts the place name (the tab strip below says that too).
        let mut strip = seg(2, Vec::new());
        for s in app.sections() {
            let active = s == app.screen.section();
            strip.targets.push((strip.spans.len(), Target::Section(s)));
            strip.spans.push(Span::styled(
                format!("{}{} ", s.key(), if active { format!(" {}", s.title()) } else { String::new() }),
                if active { t.strong_style().fg(t.focus) } else { t.dim_style() },
            ));
        }
        if let Some(last) = strip.spans.last_mut() {
            *last = Span::styled(last.content.trim_end().to_string(), last.style);
        }
        segs.push(strip);
    }
    // Where you are, beyond what the tab strip below already says (the section and its tab):
    // an open detail, with its trail. Nothing at all on a plain screen.
    let tabs = app.screen.section().tab_labels(&app.config.features).len();
    let crumbs: Vec<String> = app.breadcrumb().into_iter().skip(if tabs > 1 { 2 } else { 1 }).collect();
    let last = crumbs.len().saturating_sub(1);
    let mut trail = Vec::new();
    for (i, c) in crumbs.iter().enumerate().take(last) {
        if i > 0 {
            trail.push(Span::styled(" › ", t.dim_style()));
        }
        trail.push(Span::styled(truncate(c, 24), t.dim_style()));
    }
    let trail_at = (!trail.is_empty()).then(|| {
        trail.push(Span::styled(" › ", t.dim_style()));
        segs.push(seg(4, trail));
        segs.len() - 1
    });
    if let Some(here) = crumbs.last() {
        segs.push(Seg {
            joined: trail_at,
            ..seg(if show_screen { 3 } else { 2 }, vec![Span::styled(truncate(here, 24), t.strong_style())])
        });
    }
    let node_at = segs.len();
    // The block glyph pulses only where motion is on; with it off, a new block is just the
    // height changing.
    let beating = app.motion().effects() && app.beat.is_some_and(|b| b.elapsed() < BEAT_PULSE);
    let stale = d.health.as_ref().and_then(|h| h.head_age_secs).is_some_and(|s| s > 90);
    let mismatch = d.health.as_ref().is_some_and(|h| !h.identity_ok);
    // Every node state has its own glyph and word, never color alone.
    let (dot, word, dot_color) = match (&d.node_error, &d.health) {
        (Some(_), _) => (t.icon(Icon::Danger), "offline ", t.danger),
        _ if mismatch => (t.icon(Icon::Danger), "WRONG CHAIN ", t.danger),
        (None, None) => (t.icon(Icon::InFlight), "", t.dim),
        _ if stale => (t.icon(Icon::Stale), "stale ", t.pending),
        _ if beating => ("◉", "", t.ok),
        _ => (t.icon(Icon::On), "", darken(t.ok, 0.25)),
    };
    let net = if d.network_name.is_empty() { app.network_id.clone() } else { d.network_name.clone() };
    let mainnet = d.network_id == "mainnet" || (d.network_id.is_empty() && app.network_id == "mainnet");
    segs.push(Seg {
        targets: vec![(1, Target::Header(HeaderPart::Network))],
        ..seg(
            0,
            vec![
                Span::styled(format!("{dot} {word}"), Style::default().fg(dot_color)),
                Span::styled(net, if mainnet { Style::default().fg(t.link) } else { t.strong_style().fg(t.attention) }),
            ],
        )
    });
    if let Some(h) = &d.health {
        segs.push(Seg {
            joined: Some(node_at),
            ..seg(
                5,
                if milestone(h.height) {
                    // A round height is a small occasion: marked for the one block it lasts.
                    vec![Span::styled(format!("  ◆ #{}", amount::group_thousands(&h.height.to_string())), Style::default().fg(t.focus))]
                } else {
                    vec![Span::styled(format!("  #{}", amount::group_thousands(&h.height.to_string())), t.dim_style())]
                },
            )
        });
    }
    if let Some(e) = &d.node_error {
        segs.push(Seg {
            joined: Some(node_at),
            ..seg(3, vec![Span::styled(format!("  {}", truncate(&app::friendly_error(e), 40)), Style::default().fg(t.danger))])
        });
    }
    if let Some(m) = &app.meta {
        if m.kind == wallet_core::registry::WalletKind::Watch {
            segs.push(seg(0, vec![Span::styled(format!("{}watch-only", t.lead(Icon::Watching)), Style::default().fg(t.attention))]));
        } else if m.kind == wallet_core::registry::WalletKind::Hd && !m.backed_up {
            segs.push(seg(
                1,
                vec![Span::styled(format!("{}phrase not verified", t.lead(Icon::Attention)), Style::default().fg(t.attention))],
            ));
        }
    }
    let mut right = Vec::new();
    if let Some(b) = app.busy_text() {
        right.push(Span::styled(format!("{} {b} ", spinner()), Style::default().fg(t.pending)));
    }
    // A limit waiting for its review is a standing state, said in the header until it is.
    if let Some(n @ 1..) = app.eco.orders.as_deref().map(|rows| super::order_ui::reachable(rows).len()) {
        right.push(Span::styled(format!("◆ {} reachable ", amount::count(n, "limit")), Style::default().fg(t.attention)));
    }
    let unread = d.notifications.iter().filter(|n| !n.read).count();
    let mut unread_at = None;
    if unread > 0 {
        unread_at = Some(right.len());
        right.push(Span::styled(format!("{} {unread} unread ", t.icon(Icon::Bell)), Style::default().fg(t.attention)));
        right.push(Span::styled("N  ", t.strong_style().fg(t.focus)));
    }
    if app.can_sign() {
        match app.autolock_remaining() {
            Some(s) if app.dash.unlocked => {
                let c = if s < 60 { t.attention } else { t.ok };
                right.push(Span::styled(
                    format!(
                        "{} unlocked · {} ",
                        t.icon(Icon::Unlocked),
                        if s < 60 { format!("{s}s") } else { format!("{}m", s.div_ceil(60)) }
                    ),
                    Style::default().fg(c),
                ));
            }
            _ if app.dash.unlocked => right.push(Span::styled(format!("{} unlocked ", t.icon(Icon::Unlocked)), Style::default().fg(t.ok))),
            _ => right.push(Span::styled(format!("{} locked ", t.icon(Icon::Lock)), t.dim_style())),
        }
    }
    // The right side wins; the left side drops whole segments to fit beside it.
    let right_w = (Line::from(right.clone()).width() as u16).min(area.width);
    let left_area = Rect { width: area.width.saturating_sub(right_w + 1), ..area };
    for (i, s) in segs.iter_mut().enumerate() {
        s.id = i;
    }
    // A joined segment sits against its partner only while every segment since the partner is
    // joined to it too; otherwise it takes a separator like any other.
    let attached = |segs: &[Seg], i: usize| -> bool {
        let Some(partner) = segs[i].joined else { return false };
        i > 0 && (segs[i - 1].id == partner || segs[i - 1].joined == Some(partner))
    };
    let width_of = |segs: &[Seg]| -> usize {
        (0..segs.len()).map(|i| Line::from(segs[i].spans.clone()).width() + if i > 0 && !attached(segs, i) { 3 } else { 0 }).sum()
    };
    while width_of(&segs) > left_area.width as usize {
        let Some(worst) = segs.iter().enumerate().filter(|(_, s)| s.drop > 0).max_by_key(|(i, s)| (s.drop, *i)).map(|(i, _)| i) else {
            break;
        };
        segs.remove(worst);
    }
    let mut spans: Vec<Span> = Vec::new();
    let mut targets: Vec<(usize, Target)> = Vec::new();
    let joins: Vec<bool> = (0..segs.len()).map(|i| attached(&segs, i)).collect();
    for (i, s) in segs.into_iter().enumerate() {
        if i > 0 && !joins[i] {
            spans.push(sep());
        }
        targets.extend(s.targets.into_iter().map(|(k, target)| (spans.len() + k, target)));
        spans.extend(s.spans);
    }
    {
        let mut hits = app.hits.borrow_mut();
        hits.spans(left_area, &spans, |i| targets.iter().find(|(at, _)| *at == i).map(|(_, t)| t.clone()));
        let right_area = Rect { x: area.right().saturating_sub(right_w), width: right_w, ..area };
        hits.spans(right_area, &right, |i| unread_at.filter(|u| i == *u || i == *u + 1).map(|_| Target::Header(HeaderPart::Unread)));
    }
    let right_line = Line::from(right);
    f.render_widget(Block::default().style(Style::default().bg(app.bar_bg())), area);
    f.render_widget(Paragraph::new(Line::from(spans)), left_area);
    f.render_widget(Paragraph::new(right_line).alignment(Alignment::Right), area);
}

fn draw_nav(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    // The sections start level with the content, below the tab strip (two rows unless short).
    let top = if app.short { 1 } else { 2 };
    let rail = Rect { y: area.y + top, height: area.height.saturating_sub(top), ..area };
    let sections = app.sections();
    // A blank row between sections where the height allows it, so each is an easy target.
    let spaced = rail.height as usize >= sections.len() * 2 + 4;
    // Everything left of the rail's border; the active row is highlighted across all of it.
    let inner = rail.width.saturating_sub(1) as usize;
    let mut lines = Vec::new();
    // Line index → what a click on that line does.
    let mut targets: Vec<(usize, Target)> = Vec::new();
    let current = app.screen.section();
    for (i, s) in sections.iter().enumerate() {
        if i > 0 && (spaced || *s == app::Section::System) {
            lines.push(Line::from(""));
        }
        if i > 0 && spaced && *s == app::Section::System {
            lines.push(Line::from(""));
        }
        targets.push((lines.len(), Target::Section(*s)));
        let active = *s == current;
        let style = if active { t.selected() } else { t.text_style() };
        let dim = if active { style } else { t.dim_style() };
        let dot = match s {
            // Money arrived since Activity was last opened: marked until it is.
            app::Section::Activity if app.arrivals_unseen => Span::styled("•", style.fg(t.ok)),
            app::Section::Home => Span::styled("•", style.fg(t.quai)),
            app::Section::Nfts => Span::styled("◧", style.fg(t.qi)),
            _ => Span::styled(" ", style),
        };
        // A section's icon where the terminal draws Nerd Font icons; the title takes what's left.
        let icon = t.lead(match s {
            app::Section::Home => Icon::Home,
            app::Section::Markets => Icon::Trade,
            app::Section::Trade => Icon::Exchange,
            app::Section::Nfts => Icon::Nfts,
            app::Section::People => Icon::People,
            app::Section::Activity => Icon::Activity,
            app::Section::System => Icon::System,
        });
        // bar, " k  ", icon, title, dot, one space before the border.
        let title_w = inner.saturating_sub(1 + 4 + icon.chars().count() + 1 + 1);
        lines.push(Line::from(vec![
            Span::styled(if active { "▌" } else { " " }, Style::default().fg(t.focus)),
            Span::styled(format!(" {}  ", s.key()), dim),
            Span::styled(icon, dim),
            Span::styled(format!("{:<title_w$}", s.title()), style.add_modifier(if active { Modifier::BOLD } else { Modifier::empty() })),
            dot,
            Span::styled(" ", style),
        ]));
        // Sections only: the tab strip beside it shows the sub-tabs, once.
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("   [ ] tabs", t.dim_style().add_modifier(Modifier::ITALIC))));
    let block = Block::default().borders(Borders::RIGHT).border_type(BorderType::Plain).border_style(t.border(false));
    {
        let mut hits = app.hits.borrow_mut();
        for (line, target) in targets {
            if (line as u16) < rail.height {
                hits.add(Rect::new(rail.x, rail.y + line as u16, rail.width.saturating_sub(1), 1), target);
            }
        }
    }
    f.render_widget(Paragraph::new(lines).block(block), rail);
}

fn draw_footer(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let mut hints: Vec<(String, String)> = app::context_hints(app);
    let at_screen = matches!(app.modal, Modal::None) && !app.dock_focus && !(app.detail.is_empty() && app.input_focused());
    if at_screen && app.dash.notifications.iter().any(|n| !n.read) {
        hints.insert(0, ("N".into(), "notifications".into()));
    }
    // The way to everything else: this view's other actions, then anything anywhere; lock, quit
    // and the rest of the keys are under ?.
    let mut tail: Vec<(String, String)> = Vec::new();
    if at_screen {
        if !app.keys_here().sheet.is_empty() {
            tail.push(("space".into(), "actions".into()));
        }
        tail.push((":".into(), "palette".into()));
        tail.push(("?".into(), "more".into()));
    }
    let width = |h: &[(String, String)]| h.iter().map(|(k, v)| k.chars().count() + v.chars().count() + 3).sum::<usize>();
    // Narrow: the view's own hints give way from the end; the ways to everything else stay.
    while !hints.is_empty() && width(&hints) + width(&tail) > area.width as usize {
        hints.pop();
    }
    hints.extend(tail);
    let mut spans = Vec::new();
    let mut keys = Vec::new();
    for (k, v) in hints.iter() {
        spans.push(Span::styled(format!(" {k}"), t.strong_style().fg(t.focus)));
        spans.push(Span::styled(format!(" {v} "), t.dim_style()));
        keys.push(hint_key(k));
    }
    // A hint is its key: clicking the key or its label presses it.
    app.hits.borrow_mut().spans(area, &spans, |i| keys.get(i / 2).copied().flatten().map(Target::Key));
    f.render_widget(Paragraph::new(Line::from(spans)).style(Style::default().bg(app.bar_bg())), area);
}

/// A severity's mark and color: the glyph carries it where color can't.
pub(crate) fn severity_mark(t: &Theme, level: app::Severity) -> (&'static str, Color) {
    match level {
        app::Severity::Info => (t.icon(Icon::Info), t.link),
        app::Severity::Ok => (t.icon(Icon::Ok), t.ok),
        app::Severity::Attention => (t.icon(Icon::Attention), t.attention),
        app::Severity::Danger => (t.icon(Icon::Danger), t.danger),
    }
}

/// The key a footer hint names, when it is one key a click can press. Chords, ranges ("j/k",
/// "[ ]") and modifier keys are for the keyboard.
fn hint_key(k: &str) -> Option<crossterm::event::KeyCode> {
    use crossterm::event::KeyCode;
    match k {
        "enter" => Some(KeyCode::Enter),
        "esc" => Some(KeyCode::Esc),
        "tab" => Some(KeyCode::Tab),
        "space" => Some(KeyCode::Char(' ')),
        _ => {
            let mut chars = k.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Some(KeyCode::Char(c)),
                _ => None,
            }
        }
    }
}

/// One line in the bottom-right corner while a transaction is sent and not yet mined: a turning
/// spinner, what it is, and how long it has been waiting. It never takes a second row, and the
/// toasts stack above it.
fn draw_pending(f: &mut Frame, app: &App, t: &Theme, area: Rect) -> u16 {
    let waiting = app.confirming_ops();
    let Some(oldest) = waiting.first() else {
        // The one that just left the pill is said where the pill was, for a moment.
        let Some((text, _)) = &app.pill_resolved else { return 0 };
        let failed = text.starts_with(t.icon(Icon::Danger));
        let text = truncate(text, (area.width as usize).saturating_sub(12).min(70));
        let w = (text.chars().count() as u16 + 3).min(area.width);
        let rect = Rect::new(area.right().saturating_sub(w + 1), area.bottom().saturating_sub(1), w, 1);
        f.render_widget(Clear, rect);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("▌ ", Style::default().fg(if failed { t.danger } else { t.ok })),
                Span::styled(text, t.text_style()),
            ]))
            .style(Style::default().bg(t.raised)),
            rect,
        );
        return 1;
    };
    let more = waiting.len().saturating_sub(1);
    let summary = match more {
        0 => wallet_core::track::describe(oldest),
        n => format!("{} +{n}", wallet_core::track::describe(oldest)),
    };
    let max = (area.width as usize).saturating_sub(18).min(52);
    let text = format!("{} · {}", truncate(&summary, max), super::views::flow_age(oldest.updated));
    // A still glyph where motion is off: the age keeps counting either way.
    let glyph = if app.motion().effects() { spinner() } else { t.icon(Icon::InFlight) };
    let w = (text.chars().count() as u16 + 6).min(area.width);
    let rect = Rect::new(area.right().saturating_sub(w + 1), area.bottom().saturating_sub(1), w, 1);
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("▌", Style::default().fg(t.pending)),
            Span::styled(format!("{glyph} "), Style::default().fg(t.pending)),
            Span::styled(text, t.text_style()),
        ]))
        .style(Style::default().bg(t.raised)),
        rect,
    );
    1
}

fn draw_toasts(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    // Newest on the footer row (right side), older ones stacked above it — above the pending
    // pill when one is showing. A modal owns everything above the footer row (`centered` keeps
    // that row clear), so while one is open only the newest toast shows, on that row, and the
    // pending pill waits: nothing may land on a review's buttons.
    let modal_open = !matches!(app.modal, Modal::None);
    let pill = if modal_open { 0 } else { draw_pending(f, app, t, area) };
    let mut y = area.bottom().saturating_sub(1 + pill);
    let shown = if modal_open { 1 } else { usize::MAX };
    for toast in app.toasts.iter().rev().take(shown) {
        let max = (area.width as usize).saturating_sub(10).min(90);
        let text = truncate(&toast.text, max);
        let w = (text.chars().count() as u16 + 6).min(area.width);
        let rect = Rect::new(area.right().saturating_sub(w + 1), y, w, 1);
        app.hits.borrow_mut().add(rect, Target::Toast);
        let (glyph, color) = severity_mark(t, toast.level);
        f.render_widget(Clear, rect);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("▌", Style::default().fg(color)),
                Span::styled(format!("{glyph} "), Style::default().fg(color)),
                Span::styled(format!("{text} "), Style::default().fg(t.strong)),
            ]))
            .style(Style::default().bg(t.raised)),
            rect,
        );
        if y <= area.y + 2 {
            break;
        }
        y -= 1;
    }
}

#[cfg(test)]
#[path = "../ui_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "../ui_nft_grid_tests.rs"]
mod nft_grid;
