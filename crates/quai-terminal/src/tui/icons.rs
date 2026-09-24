//! One glyph per meaning, in three sets.
//!
//! Every icon the TUI draws is named here, with its form in each set:
//!
//! - **Nerd**: Nerd Font icons (Material Design, `nf-md-*`) where a picture says it better, and
//!   the Unicode glyph for status and marks, which already read well at text size.
//! - **Unicode**: the geometric glyphs every monospace font has (the glyph test's allow-list).
//!   Where no glyph says the thing (a section, a wallet), the form is empty and the label stands
//!   alone.
//! - **ASCII**: for the Linux console and anyone who asks.
//!
//! A glyph has one meaning. `◌` is something in flight, `◔` is something stale, `◕` is time-locked,
//! `○` is off and `●` is on; `✕` is the only error mark. Nerd icons are one cell in a "Nerd Font
//! Mono" and up to two in the proportional variants, so an icon is always followed by a space and
//! never packed against another.

use std::sync::OnceLock;

/// Which glyphs to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Set {
    Nerd,
    Unicode,
    Ascii,
}

/// Everything the TUI marks with a glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    // Status
    Ok,
    InFlight,
    Stale,
    Locked,
    Attention,
    Warning,
    Danger,
    Info,
    On,
    Off,
    // Marks
    Disclosure,
    Dropdown,
    Up,
    Down,
    // Sections
    Home,
    Trade,
    Exchange,
    Nfts,
    People,
    Activity,
    System,
    // Things
    Wallet,
    Lock,
    Unlocked,
    Watching,
    Send,
    Receive,
    Swap,
    Convert,
    Wrap,
    Unwrap,
    Approve,
    Gather,
    Notify,
    Pool,
    Launch,
    Curve,
    Protocol,
    Listed,
    Staked,
    Legacy,
    Coins,
    Chat,
    Bell,
    Search,
    Mining,
}

impl Icon {
    #[cfg(test)]
    pub const ALL: [Icon; 46] = [
        Icon::Ok,
        Icon::InFlight,
        Icon::Stale,
        Icon::Locked,
        Icon::Attention,
        Icon::Warning,
        Icon::Danger,
        Icon::Info,
        Icon::On,
        Icon::Off,
        Icon::Disclosure,
        Icon::Dropdown,
        Icon::Up,
        Icon::Down,
        Icon::Home,
        Icon::Trade,
        Icon::Exchange,
        Icon::Nfts,
        Icon::People,
        Icon::Activity,
        Icon::System,
        Icon::Wallet,
        Icon::Lock,
        Icon::Unlocked,
        Icon::Watching,
        Icon::Send,
        Icon::Receive,
        Icon::Swap,
        Icon::Convert,
        Icon::Wrap,
        Icon::Unwrap,
        Icon::Approve,
        Icon::Gather,
        Icon::Notify,
        Icon::Pool,
        Icon::Launch,
        Icon::Curve,
        Icon::Protocol,
        Icon::Listed,
        Icon::Staked,
        Icon::Legacy,
        Icon::Coins,
        Icon::Chat,
        Icon::Bell,
        Icon::Search,
        Icon::Mining,
    ];

    /// (nerd, unicode, ascii). An empty form draws nothing.
    const fn forms(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Icon::Ok => ("✓", "✓", "+"),
            Icon::InFlight => ("◌", "◌", "~"),
            // `= 1m`: as of a minute ago (a `?` read as a question; `~` is in flight).
            Icon::Stale => ("◔", "◔", "="),
            Icon::Locked => ("◕", "◕", "#"),
            Icon::Attention => ("!", "!", "!"),
            Icon::Warning => ("⚠", "⚠", "!"),
            Icon::Danger => ("✕", "✕", "x"),
            Icon::Info => ("·", "·", "-"),
            Icon::On => ("●", "●", "*"),
            Icon::Off => ("○", "○", "o"),
            Icon::Disclosure => ("›", "›", ">"),
            Icon::Dropdown => ("▾", "▾", "v"),
            Icon::Up => ("▲", "▲", "^"),
            Icon::Down => ("▼", "▼", "v"),
            Icon::Home => ("\u{f06a1}", "", ""),       // md-home_outline
            Icon::Trade => ("\u{f012a}", "", ""),      // md-chart_line
            Icon::Exchange => ("\u{f04e1}", "", ""),   // md-swap_horizontal (the Trade section)
            Icon::Nfts => ("\u{f02ef}", "", ""),       // md-image_multiple_outline
            Icon::People => ("\u{f000e}", "", ""),     // md-account_multiple
            Icon::Activity => ("\u{f02da}", "", ""),   // md-history
            Icon::System => ("\u{f08bb}", "", ""),     // md-cog_outline
            Icon::Wallet => ("\u{f0584}", "", ""),     // md-wallet
            Icon::Lock => ("\u{f033e}", "○", "o"),     // md-lock (the wallet: signing is off)
            Icon::Unlocked => ("\u{f0fc6}", "●", "*"), // md-lock_open_variant
            Icon::Watching => ("\u{f0208}", "", ""),   // md-eye
            Icon::Send => ("\u{f005c}", "↗", ">"),     // md-arrow_top_right
            Icon::Receive => ("\u{f0042}", "↘", "<"),  // md-arrow_bottom_left
            Icon::Swap => ("\u{f04e1}", "↔", "="),     // md-swap_horizontal
            Icon::Convert => ("\u{f04e1}", "↔", "="),  // md-swap_horizontal
            Icon::Wrap => ("\u{f0e66}", "+", "+"),     // md-sprout (Qi grows into WQI)
            Icon::Unwrap => ("−", "−", "-"),
            Icon::Approve => ("\u{f0565}", "±", "~"), // md-shield_check
            Icon::Gather => ("\u{f06e1}", "≋", "&"),  // md-hexagon_multiple
            Icon::Notify => ("\u{f00e6}", "@", "@"),  // md-bullhorn
            Icon::Pool => ("\u{f058c}", "", ""),      // md-water
            // A launch on its curve is a hollow diamond; graduated, it fills in.
            Icon::Launch => ("\u{f14de}", "◈", "L"), // md-rocket_launch
            Icon::Curve => ("\u{f0c50}", "◇", "c"),  // md-chart_bell_curve
            // Priced from the protocol's own conversion rate, not a market.
            Icon::Protocol => ("◊", "◊", "p"),
            Icon::Listed => ("\u{f04f9}", "$", "$"), // md-tag: for sale
            Icon::Staked => ("\u{f0565}", "■", "="), // md-shield_check: in a gauge
            Icon::Legacy => ("◦", "◦", "q"),         // the older QuaiSwap exchange
            Icon::Coins => ("\u{f0b38}", "◎", "0"),  // md-circle_multiple
            Icon::Chat => ("\u{f0369}", "", ""),     // md-message_text
            Icon::Bell => ("\u{f009c}", "●", "*"),   // md-bell_outline
            Icon::Search => ("\u{f0349}", "", ""),   // md-magnify
            Icon::Mining => ("\u{f08b7}", "", ""),   // md-pickaxe
        }
    }

    /// The glyph in a set (may be empty).
    pub fn glyph(self, set: Set) -> &'static str {
        let (nerd, unicode, ascii) = self.forms();
        match set {
            Set::Nerd => nerd,
            Set::Unicode => unicode,
            Set::Ascii => ascii,
        }
    }

    /// The glyph and the space after it, or nothing when the set has no glyph for this.
    pub fn lead(self, set: Set) -> String {
        match self.glyph(set) {
            "" => String::new(),
            g => format!("{g} "),
        }
    }
}

/// What the probe found: whether Nerd Font icons draw here.
static NERD: OnceLock<bool> = OnceLock::new();

/// Whether the probe found Nerd Font icons (false until it answers, and in tests, which never
/// probe: goldens are drawn with the Unicode set whatever terminal runs them).
pub fn nerd_detected() -> bool {
    NERD.get().copied().unwrap_or(false)
}

/// Ask once, off the render path, whether Nerd Font icons will draw. There is no protocol that
/// says which font a terminal renders with, so: kitty (0.36+), ghostty and WezTerm bundle the
/// symbols as a fallback; elsewhere fontconfig is asked whether any installed font covers them
/// (kitty, foot and Alacritty all fall back through it). Over SSH the font is on the other
/// machine, and the Linux console has none: no.
pub fn probe() {
    std::thread::spawn(|| {
        let remote = std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some();
        let console = std::env::var("TERM").is_ok_and(|t| t == "linux");
        let bundled = std::env::var_os("KITTY_WINDOW_ID").is_some()
            || std::env::var("TERM").is_ok_and(|t| t == "xterm-kitty" || t == "xterm-ghostty" || t == "wezterm")
            || std::env::var("TERM_PROGRAM").is_ok_and(|p| p == "ghostty" || p == "WezTerm");
        let found = !remote
            && !console
            && (bundled
                || std::process::Command::new("fc-list")
                    .args([":charset=f0584", "family"])
                    .stderr(std::process::Stdio::null())
                    .output()
                    .is_ok_and(|out| out.status.success() && !out.stdout.trim_ascii().is_empty()));
        let _ = NERD.set(found);
        super::term::wake();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every icon has a form in every set that belongs to it: Unicode inside the glyph allow-list,
    /// ASCII in ASCII, Nerd either the Unicode glyph or a Nerd Font icon (private use area).
    #[test]
    fn every_icon_has_all_three_forms() {
        let allowed = super::super::ui::tests::GLYPHS;
        for icon in Icon::ALL {
            let (nerd, unicode, ascii) = icon.forms();
            assert!(unicode.chars().all(|c| c.is_ascii() || allowed.contains(c)), "{icon:?} unicode {unicode:?}");
            assert!(ascii.is_ascii(), "{icon:?} ascii {ascii:?}");
            assert!(!nerd.is_empty(), "{icon:?} has no Nerd form");
            assert!(
                nerd.chars().all(|c| c.is_ascii()
                    || allowed.contains(c)
                    || ('\u{f0000}'..='\u{fffff}').contains(&c)
                    || ('\u{e000}'..='\u{f8ff}').contains(&c)),
                "{icon:?} nerd {nerd:?}"
            );
            // An empty Unicode form must not leave the ASCII one saying something the others don't.
            if unicode.is_empty() {
                assert!(ascii.is_empty(), "{icon:?}");
            }
        }
    }

    /// One meaning per glyph among the status marks.
    #[test]
    fn status_glyphs_are_distinct() {
        let status = [Icon::Ok, Icon::InFlight, Icon::Stale, Icon::Locked, Icon::Attention, Icon::Danger, Icon::On, Icon::Off];
        // (Warning is Attention, louder: they share `!` in ASCII by design.)
        for set in [Set::Unicode, Set::Ascii] {
            let mut seen: Vec<&str> = status.iter().map(|i| i.glyph(set)).collect();
            seen.sort_unstable();
            let n = seen.len();
            seen.dedup();
            assert_eq!(n, seen.len(), "{set:?}");
        }
    }
}
