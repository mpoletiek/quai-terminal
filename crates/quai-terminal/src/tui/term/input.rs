//! termina's events in the app's vocabulary.
//!
//! The app, its tests and its key tables are written in crossterm's key and mouse types; termina
//! reads the terminal (it parses the replies to capability queries instead of typing them into
//! the UI). This is the one place the two meet. Terminal replies never reach the app: they are
//! answered during the startup probe, and any that arrive later are dropped here.

use crossterm::event as ct;
use termina::Event as TEvent;
use termina::event as te;

/// What the loop hands the app: an input event, or nothing worth delivering.
pub fn convert(event: TEvent) -> Option<ct::Event> {
    Some(match event {
        TEvent::Key(k) => ct::Event::Key(key(k)?),
        TEvent::Mouse(m) => ct::Event::Mouse(mouse(m)),
        TEvent::WindowResized(size) => ct::Event::Resize(size.cols, size.rows),
        TEvent::FocusIn => ct::Event::FocusGained,
        TEvent::FocusOut => ct::Event::FocusLost,
        TEvent::Paste(text) => ct::Event::Paste(text),
        // Replies to queries (colors, modes, device attributes): never input.
        TEvent::Csi(_) | TEvent::Osc(_) | TEvent::Dcs(_) => return None,
    })
}

fn modifiers(m: te::Modifiers) -> ct::KeyModifiers {
    let mut out = ct::KeyModifiers::NONE;
    for (from, to) in [
        (te::Modifiers::SHIFT, ct::KeyModifiers::SHIFT),
        (te::Modifiers::ALT, ct::KeyModifiers::ALT),
        (te::Modifiers::CONTROL, ct::KeyModifiers::CONTROL),
        (te::Modifiers::SUPER, ct::KeyModifiers::SUPER),
        (te::Modifiers::HYPER, ct::KeyModifiers::HYPER),
        (te::Modifiers::META, ct::KeyModifiers::META),
    ] {
        if m.contains(from) {
            out |= to;
        }
    }
    out
}

fn key(k: te::KeyEvent) -> Option<ct::KeyEvent> {
    let code = match k.code {
        te::KeyCode::Char(c) => ct::KeyCode::Char(c),
        te::KeyCode::Enter => ct::KeyCode::Enter,
        te::KeyCode::Backspace => ct::KeyCode::Backspace,
        te::KeyCode::Tab => ct::KeyCode::Tab,
        te::KeyCode::Escape => ct::KeyCode::Esc,
        te::KeyCode::Left => ct::KeyCode::Left,
        te::KeyCode::Right => ct::KeyCode::Right,
        te::KeyCode::Up => ct::KeyCode::Up,
        te::KeyCode::Down => ct::KeyCode::Down,
        te::KeyCode::Home => ct::KeyCode::Home,
        te::KeyCode::End => ct::KeyCode::End,
        te::KeyCode::BackTab => ct::KeyCode::BackTab,
        te::KeyCode::PageUp => ct::KeyCode::PageUp,
        te::KeyCode::PageDown => ct::KeyCode::PageDown,
        te::KeyCode::Insert => ct::KeyCode::Insert,
        te::KeyCode::Delete => ct::KeyCode::Delete,
        te::KeyCode::KeypadBegin => ct::KeyCode::KeypadBegin,
        te::KeyCode::CapsLock => ct::KeyCode::CapsLock,
        te::KeyCode::ScrollLock => ct::KeyCode::ScrollLock,
        te::KeyCode::NumLock => ct::KeyCode::NumLock,
        te::KeyCode::PrintScreen => ct::KeyCode::PrintScreen,
        te::KeyCode::Pause => ct::KeyCode::Pause,
        te::KeyCode::Menu => ct::KeyCode::Menu,
        te::KeyCode::Null => ct::KeyCode::Null,
        te::KeyCode::Function(n) => ct::KeyCode::F(n),
        // A modifier on its own (kitty reports these with some flags) or a media key does
        // nothing here.
        te::KeyCode::Modifier(_) | te::KeyCode::Media(_) => return None,
    };
    let kind = match k.kind {
        te::KeyEventKind::Press => ct::KeyEventKind::Press,
        te::KeyEventKind::Release => ct::KeyEventKind::Release,
        te::KeyEventKind::Repeat => ct::KeyEventKind::Repeat,
    };
    let mut modifiers = modifiers(k.modifiers);
    // BackTab carries SHIFT from some sources and not others; the app matches BackTab alone.
    if code == ct::KeyCode::BackTab {
        modifiers.remove(ct::KeyModifiers::SHIFT);
    }
    Some(ct::KeyEvent { code, modifiers, kind, state: ct::KeyEventState::NONE })
}

fn button(b: te::MouseButton) -> ct::MouseButton {
    match b {
        te::MouseButton::Left => ct::MouseButton::Left,
        te::MouseButton::Right => ct::MouseButton::Right,
        te::MouseButton::Middle => ct::MouseButton::Middle,
    }
}

fn mouse(m: te::MouseEvent) -> ct::MouseEvent {
    let kind = match m.kind {
        te::MouseEventKind::Down(b) => ct::MouseEventKind::Down(button(b)),
        te::MouseEventKind::Up(b) => ct::MouseEventKind::Up(button(b)),
        te::MouseEventKind::Drag(b) => ct::MouseEventKind::Drag(button(b)),
        te::MouseEventKind::Moved => ct::MouseEventKind::Moved,
        te::MouseEventKind::ScrollDown => ct::MouseEventKind::ScrollDown,
        te::MouseEventKind::ScrollUp => ct::MouseEventKind::ScrollUp,
        te::MouseEventKind::ScrollLeft => ct::MouseEventKind::ScrollLeft,
        te::MouseEventKind::ScrollRight => ct::MouseEventKind::ScrollRight,
    };
    ct::MouseEvent { kind, column: m.column, row: m.row, modifiers: modifiers(m.modifiers) }
}

/// Collapse a burst of events for one frame: of consecutive pointer moves (and drags with the
/// same button) only the last is kept, since each would draw a frame and only the last position
/// is ever seen. Everything else keeps its place and order.
pub fn coalesce(events: Vec<ct::Event>) -> Vec<ct::Event> {
    let mut out: Vec<ct::Event> = Vec::with_capacity(events.len());
    for e in events {
        if let (Some(ct::Event::Mouse(last)), ct::Event::Mouse(next)) = (out.last(), &e) {
            let same = matches!((last.kind, next.kind), (ct::MouseEventKind::Moved, ct::MouseEventKind::Moved))
                || matches!((last.kind, next.kind), (ct::MouseEventKind::Drag(a), ct::MouseEventKind::Drag(b)) if a == b);
            if same {
                out.pop();
            }
        }
        out.push(e);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tkey(code: te::KeyCode, m: te::Modifiers) -> TEvent {
        TEvent::Key(te::KeyEvent::new(code, m))
    }

    #[test]
    fn keys_arrive_as_the_app_knows_them() {
        let e = convert(tkey(te::KeyCode::Escape, te::Modifiers::NONE)).unwrap();
        assert_eq!(e, ct::Event::Key(ct::KeyEvent::new(ct::KeyCode::Esc, ct::KeyModifiers::NONE)));
        let e = convert(tkey(te::KeyCode::Char('c'), te::Modifiers::CONTROL)).unwrap();
        assert_eq!(e, ct::Event::Key(ct::KeyEvent::new(ct::KeyCode::Char('c'), ct::KeyModifiers::CONTROL)));
        let e = convert(tkey(te::KeyCode::Char('S'), te::Modifiers::SHIFT)).unwrap();
        assert_eq!(e, ct::Event::Key(ct::KeyEvent::new(ct::KeyCode::Char('S'), ct::KeyModifiers::SHIFT)));
        let e = convert(tkey(te::KeyCode::BackTab, te::Modifiers::SHIFT)).unwrap();
        assert_eq!(e, ct::Event::Key(ct::KeyEvent::new(ct::KeyCode::BackTab, ct::KeyModifiers::NONE)));
        assert!(convert(tkey(te::KeyCode::Modifier(te::ModifierKeyCode::LeftShift), te::Modifiers::SHIFT)).is_none());
    }

    #[test]
    fn replies_to_queries_never_become_input() {
        assert!(convert(TEvent::Csi(termina::escape::csi::Csi::Keyboard(termina::escape::csi::Keyboard::QueryFlags))).is_none());
    }

    #[test]
    fn a_burst_of_moves_draws_once() {
        let moved =
            |x| ct::Event::Mouse(ct::MouseEvent { kind: ct::MouseEventKind::Moved, column: x, row: 1, modifiers: ct::KeyModifiers::NONE });
        let click = ct::Event::Mouse(ct::MouseEvent {
            kind: ct::MouseEventKind::Down(ct::MouseButton::Left),
            column: 9,
            row: 1,
            modifiers: ct::KeyModifiers::NONE,
        });
        let key = ct::Event::Key(ct::KeyEvent::new(ct::KeyCode::Char('j'), ct::KeyModifiers::NONE));
        let out = coalesce(vec![moved(1), moved(2), moved(3), click.clone(), moved(4), key.clone(), moved(5), moved(6)]);
        assert_eq!(out, vec![moved(3), click, moved(4), key, moved(6)], "moves collapse; clicks and keys keep their order");
    }
}
