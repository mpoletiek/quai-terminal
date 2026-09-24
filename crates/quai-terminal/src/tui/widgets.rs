//! The component kit: pieces every screen draws the same way.
//!
//! Each returns spans (or draws into a buffer) styled from the theme's roles, so a screen says
//! *what* it shows and the kit decides how it looks. Screens adopt these as they are rebuilt.

use super::theme::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

/// An address to be read and checked: every `0x` + 40 hex digits in `text` is split into groups
/// of four, alternating bright and plain, so the eye can walk it group by group against another
/// copy (the review, the receive screen). Everything else in `text` keeps `rest`.
pub fn address(t: &Theme, text: &str, rest: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut plain = String::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < text.len() {
        let is_addr = text[i..].starts_with("0x")
            && bytes.len() >= i + 42
            && bytes[i + 2..i + 42].iter().all(u8::is_ascii_hexdigit)
            && !bytes.get(i + 42).is_some_and(u8::is_ascii_hexdigit);
        if !is_addr {
            let ch = text[i..].chars().next().expect("in bounds");
            plain.push(ch);
            i += ch.len_utf8();
            continue;
        }
        if !plain.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut plain), rest));
        }
        spans.push(Span::styled("0x", t.dim_style()));
        for (g, chunk) in bytes[i + 2..i + 42].chunks(4).enumerate() {
            let group = String::from_utf8_lossy(chunk).into_owned();
            let style = if g % 2 == 0 { t.strong_style() } else { t.text_style() };
            spans.push(Span::styled(format!(" {group}"), style));
        }
        i += 42;
    }
    if !plain.is_empty() {
        spans.push(Span::styled(plain, rest));
    }
    spans
}

/// The label column of a key/value list: one width everywhere, so details line up from screen
/// to screen. A label longer than this still fits (the value moves right) and is kept short.
pub const KV_LABEL: usize = 14;

/// A key/value row: the label quiet in a fixed column, then the value's spans.
pub fn kv<'a>(t: &Theme, label: &str, value: Vec<Span<'a>>) -> ratatui::text::Line<'a> {
    // A label as wide as the column still gets a space before its value ("effective price0.01").
    let padded = if label.chars().count() >= KV_LABEL { format!("{label} ") } else { format!("{label:<KV_LABEL$}") };
    let mut spans = vec![Span::styled(padded, t.dim_style())];
    spans.extend(value);
    ratatui::text::Line::from(spans)
}

/// A key/value line broken to `width`, its continuation lines indented to the value column.
pub fn hang(line: ratatui::text::Line<'static>, width: usize) -> Vec<ratatui::text::Line<'static>> {
    let indent = KV_LABEL;
    if line.width() <= width || width <= indent + 8 {
        return vec![line];
    }
    let style = line.spans.last().map(|s| s.style).unwrap_or_default();
    let label = line.spans.first().cloned();
    let value: String = line.spans.iter().skip(1).map(|s| s.content.as_ref()).collect();
    let mut out = Vec::new();
    let mut current = String::new();
    for word in value.split(' ') {
        if !current.is_empty() && current.chars().count() + 1 + word.chars().count() > width - indent {
            out.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    out.push(current);
    out.into_iter()
        .enumerate()
        .map(|(i, text)| {
            let lead = if i == 0 { label.clone().unwrap_or_default() } else { Span::raw(" ".repeat(indent)) };
            ratatui::text::Line::from(vec![lead, Span::styled(text, style)])
        })
        .collect()
}

/// A filled label: `color` as the fill, black or white ink (whichever reads), a cell of padding
/// each side. On the ANSI palette it is reversed text.
pub fn pill(t: &Theme, text: &str, color: ratatui::style::Color) -> Span<'static> {
    Span::styled(format!(" {text} "), t.chip(color))
}

/// How a button stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonState {
    /// Where enter lands: filled.
    Focused,
    /// Available, not focused: its color, unfilled.
    Ready,
    /// Not yet: faint, and the label says why (never struck through, which reads "cancelled").
    Waiting,
}

/// A button: `label`, then the key that presses it. `color` is its meaning (`ok` to approve,
/// `danger` to delete, `text` for a plain one).
pub fn button(t: &Theme, label: &str, key: &str, color: ratatui::style::Color, state: ButtonState) -> Span<'static> {
    let text = if key.is_empty() { format!("  {label}  ") } else { format!("  {label} · {key}  ") };
    let style = match state {
        ButtonState::Focused => t.chip(color),
        ButtonState::Ready => Style::default().fg(color).add_modifier(Modifier::BOLD),
        ButtonState::Waiting => t.dim_style(),
    };
    Span::styled(text, style)
}

/// A sequence of steps on one line: done ones checked, the one under way marked, the rest to
/// come quiet, joined by rules.
pub fn stepper(t: &Theme, steps: &[(String, super::eco::StepState)]) -> Vec<Span<'static>> {
    use super::eco::StepState;
    use super::icons::Icon;
    let mut out = Vec::new();
    for (i, (name, state)) in steps.iter().enumerate() {
        if i > 0 {
            out.push(Span::styled(" ── ", Style::default().fg(t.line)));
        }
        out.push(match state {
            StepState::Done => Span::styled(format!("{} {name}", t.icon(Icon::Ok)), Style::default().fg(t.ok)),
            StepState::Now => Span::styled(format!("{} {name}", t.icon(Icon::InFlight)), t.strong_style().fg(t.pending)),
            StepState::Next => Span::styled(name.clone(), t.dim_style()),
        });
    }
    out
}

/// A horizontal meter `width` cells wide: `ratio` of it filled in `color`, the rest a quiet
/// track in `line_strong` (a boundary, so it holds 3:1).
pub fn meter(t: &Theme, ratio: f64, width: usize, color: ratatui::style::Color) -> Vec<Span<'static>> {
    let filled = (ratio.clamp(0.0, 1.0) * width as f64).round() as usize;
    vec![
        Span::styled("█".repeat(filled), Style::default().fg(color)),
        Span::styled("░".repeat(width - filled), Style::default().fg(t.line_strong)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn a_long_value_wraps_under_itself() {
        let t = Theme::terminal(false);
        let line = kv(&t, "holds", vec![Span::raw("0 QUAI accounts · 1 address watched · a Qi payment code")]);
        let lines = hang(line.clone(), 40);
        assert!(lines.len() > 1, "wrapped");
        assert!(lines.iter().all(|l| l.width() <= 40), "each fits: {lines:?}");
        assert!(text(&lines[1].spans).starts_with(&" ".repeat(KV_LABEL)), "continuation hangs at the value column");
        let joined: Vec<String> = lines.iter().map(|l| text(&l.spans)[KV_LABEL..].to_string()).collect();
        assert_eq!(joined.join(" "), "0 QUAI accounts · 1 address watched · a Qi payment code");
        assert_eq!(hang(line, 200).len(), 1, "a line that fits is left alone");
    }

    #[test]
    fn addresses_are_read_in_groups_of_four() {
        let t = Theme::terminal(false);
        let a = "0x00F41a2B3c4D5e6F7a8B9c0D1e2F3a4B5c6D804B";
        let spans = address(&t, &format!("alice · {a}"), t.text_style());
        assert_eq!(text(&spans), "alice · 0x 00F4 1a2B 3c4D 5e6F 7a8B 9c0D 1e2F 3a4B 5c6D 804B");
        // Neighbouring groups differ in weight.
        let groups: Vec<&Span> = spans.iter().filter(|s| s.content.starts_with(' ') && s.content.len() == 5).collect();
        assert_eq!(groups.len(), 10);
        assert_ne!(groups[0].style, groups[1].style);
        // Not an address: left alone.
        assert_eq!(text(&address(&t, "0x1234 and PM8TJabc", t.text_style())), "0x1234 and PM8TJabc");
    }

    #[test]
    fn a_waiting_button_says_why_without_striking_through() {
        let t = Theme::terminal(false);
        let b = button(&t, "Approve & sign", "read to enable", t.ok, ButtonState::Waiting);
        assert!(b.content.contains("read to enable"));
        assert!(!b.style.add_modifier.contains(Modifier::CROSSED_OUT));
    }

    #[test]
    fn a_meter_is_as_wide_as_asked() {
        let t = Theme::terminal(false);
        for r in [0.0, 0.33, 1.0, 2.0] {
            assert_eq!(text(&meter(&t, r, 14, t.ok)).chars().count(), 14);
        }
    }
}
