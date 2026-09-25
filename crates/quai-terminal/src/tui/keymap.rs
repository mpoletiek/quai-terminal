//! The keymap: one table, four layers.
//!
//! 1. **Input** — a focused field (an amount, a search, a filter) owns the printable keys; Esc
//!    leaves it.
//! 2. **Overlay** — a modal, the action sheet, the go-to popup: j/k move, Enter chooses, Esc
//!    closes, and letters are the overlay's own.
//! 3. **Screen** — the verbs below, each meaning one thing everywhere. A screen implements the
//!    verbs that apply to it (`verbs.rs`); everything else it can do is in its action sheet
//!    (space, or right-click), where letters are free again.
//! 4. **App** — what a verb means when the screen doesn't use it.
//!
//! The footer, Help, the palette's key column and the action sheets are all generated from
//! these tables, so what the screen says a key does is what it does. A test fails when two
//! bindings in one layer share a key.

use super::app::Screen;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A reserved verb: the same meaning on every screen that uses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Verb {
    // Navigation
    Section(u8),
    TabPrev,
    TabNext,
    PaneNext,
    PanePrev,
    Up,
    Down,
    Left,
    Right,
    Top,
    Bottom,
    PageUp,
    PageDown,
    Jump,
    Filter,
    Open,
    Back,
    /// The screen before this one.
    Previous,
    GoTo,
    // App
    Palette,
    Help,
    Notifications,
    Wallets,
    /// The account that acts (`@`).
    Account,
    Privacy,
    Lock,
    Quit,
    Sheet,
    Dock,
    // Money
    Send,
    Receive,
    Trade,
    Buy,
    Sell,
    Convert,
    Wrap,
    // Items
    Add,
    Edit,
    Remove,
    Copy,
    CopyLink,
    // View
    Flip,
    Sort,
    ViewMode,
    Reload,
    RefreshAll,
}

/// A key as the table names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Code(KeyCode),
}

impl Key {
    pub fn matches(self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match self {
            Key::Char(c) => !ctrl && key.code == KeyCode::Char(c),
            Key::Ctrl(c) => ctrl && key.code == KeyCode::Char(c),
            Key::Code(code) => !ctrl && key.code == code,
        }
    }

    /// How the key is written in hints and Help.
    pub fn label(self) -> String {
        match self {
            Key::Char(' ') => "space".into(),
            Key::Char(c) => c.to_string(),
            Key::Ctrl(c) => format!("ctrl-{c}"),
            Key::Code(KeyCode::Enter) => "enter".into(),
            Key::Code(KeyCode::Esc) => "esc".into(),
            Key::Code(KeyCode::Tab) => "tab".into(),
            Key::Code(KeyCode::BackTab) => "shift-tab".into(),
            Key::Code(KeyCode::Up) => "↑".into(),
            Key::Code(KeyCode::Down) => "↓".into(),
            Key::Code(KeyCode::Left) => "←".into(),
            Key::Code(KeyCode::Right) => "→".into(),
            Key::Code(KeyCode::Home) => "home".into(),
            Key::Code(KeyCode::End) => "end".into(),
            Key::Code(KeyCode::Backspace) => "backspace".into(),
            Key::Code(code) => format!("{code:?}").to_lowercase(),
        }
    }
}

/// What a group of bindings is for (Help's headings).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Move,
    Go,
    App,
    Money,
    Item,
    View,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::Move => "move",
            Group::Go => "go",
            Group::App => "anywhere",
            Group::Money => "money",
            Group::Item => "the focused item",
            Group::View => "the view",
        }
    }
}

/// One binding: keys, the verb they mean, and what Help calls it.
pub struct Binding {
    pub keys: &'static [Key],
    pub verb: Verb,
    pub label: &'static str,
    pub group: Group,
}

macro_rules! bind {
    ($group:ident, [$($k:expr),+], $verb:expr, $label:expr) => {
        Binding { keys: &[$($k),+], verb: $verb, label: $label, group: Group::$group }
    };
}

use Key::{Char as C, Code, Ctrl};

/// The screen and app layers. Section digits come from the section table (`Section::key`).
pub const GLOBAL: &[Binding] = &[
    bind!(Move, [C('j'), Code(KeyCode::Down)], Verb::Down, "down"),
    bind!(Move, [C('k'), Code(KeyCode::Up)], Verb::Up, "up"),
    bind!(Move, [C('h'), Code(KeyCode::Left)], Verb::Left, "left"),
    bind!(Move, [C('l'), Code(KeyCode::Right)], Verb::Right, "right"),
    bind!(Move, [Code(KeyCode::Home)], Verb::Top, "first"),
    bind!(Move, [C('G'), Code(KeyCode::End)], Verb::Bottom, "last"),
    bind!(Move, [Ctrl('d'), Code(KeyCode::PageDown)], Verb::PageDown, "page down"),
    bind!(Move, [Ctrl('u'), Code(KeyCode::PageUp)], Verb::PageUp, "page up"),
    bind!(Move, [C('\'')], Verb::Jump, "jump to a labeled row"),
    bind!(Move, [Code(KeyCode::Tab)], Verb::PaneNext, "next pane"),
    bind!(Move, [Code(KeyCode::BackTab)], Verb::PanePrev, "previous pane"),
    bind!(Go, [C('[')], Verb::TabPrev, "previous tab"),
    bind!(Go, [C(']')], Verb::TabNext, "next tab"),
    bind!(Go, [C('g')], Verb::GoTo, "go to… (g g: first row)"),
    bind!(Go, [Code(KeyCode::Enter)], Verb::Open, "open"),
    bind!(Go, [Code(KeyCode::Esc)], Verb::Back, "back"),
    bind!(Go, [Code(KeyCode::Backspace), Ctrl('o')], Verb::Previous, "the screen before"),
    bind!(App, [C(':'), Ctrl('p')], Verb::Palette, "command palette"),
    bind!(App, [C('?')], Verb::Help, "keys"),
    bind!(App, [C(' ')], Verb::Sheet, "actions for this"),
    bind!(App, [C('N')], Verb::Notifications, "notifications"),
    bind!(App, [C('W')], Verb::Wallets, "wallets"),
    bind!(App, [C('@')], Verb::Account, "the account that acts"),
    bind!(App, [C('$')], Verb::Privacy, "hide or show the balance"),
    bind!(App, [C('`')], Verb::Dock, "the pinned chat"),
    bind!(App, [Ctrl('l')], Verb::Lock, "lock"),
    bind!(App, [C('q')], Verb::Quit, "quit"),
    bind!(Money, [C('s')], Verb::Send, "send"),
    bind!(Money, [C('r')], Verb::Receive, "receive"),
    bind!(Money, [C('t')], Verb::Trade, "trade"),
    bind!(Money, [C('b')], Verb::Buy, "buy"),
    bind!(Money, [C('S')], Verb::Sell, "sell"),
    bind!(Money, [C('c')], Verb::Convert, "convert QUAI ↔ Qi"),
    bind!(Money, [C('w')], Verb::Wrap, "wrap"),
    bind!(Item, [C('a')], Verb::Add, "add"),
    bind!(Item, [C('e')], Verb::Edit, "edit"),
    bind!(Item, [C('x')], Verb::Remove, "remove"),
    bind!(Item, [C('y')], Verb::Copy, "copy"),
    bind!(Item, [C('Y')], Verb::CopyLink, "copy link"),
    bind!(View, [C('/')], Verb::Filter, "search"),
    bind!(View, [C('f')], Verb::Flip, "flip"),
    bind!(View, [C(',')], Verb::Sort, "sort"),
    bind!(View, [C('.')], Verb::ViewMode, "view"),
    bind!(View, [C('R')], Verb::Reload, "reload"),
    bind!(View, [Ctrl('r')], Verb::RefreshAll, "refresh everything"),
];

/// The movement letters, which the arrows always stand in for.
pub const VIM_KEYS: [char; 4] = ['h', 'j', 'k', 'l'];

/// The verb a key means, or none. With `vim` off, h j k l move nothing and are left to the view.
pub fn resolve(key: &KeyEvent, section_keys: &[char], vim: bool) -> Option<Verb> {
    if let KeyCode::Char(c) = key.code
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && let Some(i) = section_keys.iter().position(|k| *k == c)
    {
        return Some(Verb::Section(i as u8));
    }
    if !vim && matches!(key.code, KeyCode::Char(c) if VIM_KEYS.contains(&c)) && !key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    GLOBAL.iter().find(|b| b.keys.iter().any(|k| k.matches(key))).map(|b| b.verb)
}

/// The binding for a verb.
pub fn binding(verb: Verb) -> Option<&'static Binding> {
    GLOBAL.iter().find(|b| b.verb == verb)
}

/// The key that means a verb, as written in hints.
pub fn key_of(verb: Verb) -> String {
    binding(verb).and_then(|b| b.keys.first()).map(|k| k.label()).unwrap_or_default()
}

/// `g` then a letter: straight to a screen. One table, so every hint that names a place is
/// computed from it and can't point somewhere else.
pub const ROUTES: &[(char, Screen)] = &[
    ('h', Screen::Home),
    ('q', Screen::Qi),
    ('A', Screen::Accounts),
    ('m', Screen::Markets),
    ('x', Screen::Swap),
    ('c', Screen::Convert),
    ('w', Screen::Wrap),
    ('o', Screen::Orders),
    ('p', Screen::Pools),
    ('l', Screen::Launches),
    ('$', Screen::Pnl),
    ('n', Screen::Collected),
    ('e', Screen::Explore),
    ('i', Screen::Listings),
    ('f', Screen::Contacts),
    ('C', Screen::Channels),
    ('b', Screen::Board),
    ('a', Screen::Activity),
    ('W', Screen::Wallets),
    ('N', Screen::Network),
    ('s', Screen::Settings),
    ('d', Screen::DataSources),
];

/// The chord that goes to a screen: "g m".
pub fn chord(screen: Screen) -> String {
    ROUTES.iter().find(|(_, s)| *s == screen).map(|(c, _)| format!("g {c}")).unwrap_or_default()
}

/// The screen a go-to letter names.
pub fn route(c: char) -> Option<Screen> {
    ROUTES.iter().find(|(k, _)| *k == c).map(|(_, s)| *s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No key means two things in the same layer, and no route letter is used twice.
    #[test]
    fn keymap_has_no_conflicts() {
        let mut seen: Vec<(Key, Verb)> = Vec::new();
        for b in GLOBAL {
            for k in b.keys {
                if let Some((_, other)) = seen.iter().find(|(s, _)| s == k) {
                    panic!("{k:?} means both {other:?} and {:?}", b.verb);
                }
                seen.push((*k, b.verb));
            }
        }
        // Section digits never collide with a verb.
        for d in "0123456789".chars() {
            assert!(!seen.iter().any(|(k, _)| *k == Key::Char(d)), "{d} is a section key");
        }
        let mut letters: Vec<char> = ROUTES.iter().map(|(c, _)| *c).collect();
        letters.sort_unstable();
        let before = letters.len();
        letters.dedup();
        assert_eq!(before, letters.len(), "a go-to letter names two screens");
        for s in Screen::ALL {
            assert!(!chord(s).is_empty(), "{s:?} has no g-chord");
        }
    }

    /// `l` moves right. It used to lock the wallet everywhere but the grids.
    #[test]
    fn l_moves_and_ctrl_l_locks() {
        let l = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE);
        assert_eq!(resolve(&l, &[], true), Some(Verb::Right));
        let lock = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(resolve(&lock, &[], true), Some(Verb::Lock));
    }

    /// With the movement letters off, the arrows still move and h j k l mean nothing here.
    #[test]
    fn arrows_move_when_the_letters_do_not() {
        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
        for c in VIM_KEYS {
            assert_eq!(resolve(&press(KeyCode::Char(c)), &[], false), None, "{c}");
        }
        assert_eq!(resolve(&press(KeyCode::Down), &[], false), Some(Verb::Down));
        assert_eq!(resolve(&press(KeyCode::Left), &[], false), Some(Verb::Left));
        assert_eq!(resolve(&KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL), &[], false), Some(Verb::Lock));
    }
}
