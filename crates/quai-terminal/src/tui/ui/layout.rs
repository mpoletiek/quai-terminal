//! Size classes, and what each screen does with the room it is given.
//!
//! The frame decides the class once per frame from the terminal's size; screens ask it rather
//! than each measuring a threshold of its own for the same question. Thresholds inside one panel
//! (drop a column when the table is narrow) stay with the panel: they answer a different
//! question, about the panel's own width.

use super::super::app::{App, Screen};
use ratatui::layout::{Constraint, Layout, Rect};

/// How much room the terminal gives, across.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Breakpoint {
    /// Under 100 columns: no rail (the header carries the sections), one thing beside another
    /// only where it must be.
    Compact,
    /// The everyday size.
    #[default]
    Regular,
    /// 160 columns and up: a list screen shows its selected row in an inspector beside it.
    Wide,
}

impl Breakpoint {
    pub fn of(width: u16) -> Self {
        match width {
            ..100 => Breakpoint::Compact,
            100..160 => Breakpoint::Regular,
            _ => Breakpoint::Wide,
        }
    }
}

/// Fewer rows than this and a screen's headline shrinks to one line, so the lists under it keep
/// their rows (80×24 is the case this is for).
pub const SHORT_ROWS: u16 = 30;

/// The inspector column's width: an address in groups of four fits on two lines, and a QR code
/// for it fits across.
pub const INSPECTOR_W: u16 = 50;

/// What a screen lays out, beyond its main pane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PaneSpec {
    /// At `Wide`, the selected row's detail goes in a column beside the list. Screens that
    /// already show their detail beside the list at every width (Activity, Contacts, Channels)
    /// do not need one.
    pub inspector: bool,
}

pub fn spec(screen: Screen) -> PaneSpec {
    PaneSpec { inspector: matches!(screen, Screen::Accounts | Screen::Wallets | Screen::Orders) }
}

/// The main pane and, when this screen has one and there is room for it, its inspector. The
/// pinned chat and the trader layout already use the width, so neither leaves room.
pub fn with_inspector(app: &App, area: Rect) -> (Rect, Option<Rect>) {
    let room = app.term.breakpoint == Breakpoint::Wide && !app.dock.shown && !app.trader && area.width >= INSPECTOR_W + 60;
    if room && spec(app.nav.screen).inspector {
        let [main, inspector] = Layout::horizontal([Constraint::Min(60), Constraint::Length(INSPECTOR_W)]).areas(area);
        (main, Some(inspector))
    } else {
        (area, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakpoints_split_at_one_hundred_and_one_sixty() {
        assert_eq!(Breakpoint::of(80), Breakpoint::Compact);
        assert_eq!(Breakpoint::of(99), Breakpoint::Compact);
        assert_eq!(Breakpoint::of(100), Breakpoint::Regular);
        assert_eq!(Breakpoint::of(159), Breakpoint::Regular);
        assert_eq!(Breakpoint::of(160), Breakpoint::Wide);
        assert!(Breakpoint::Compact < Breakpoint::Regular && Breakpoint::Regular < Breakpoint::Wide);
    }
}
