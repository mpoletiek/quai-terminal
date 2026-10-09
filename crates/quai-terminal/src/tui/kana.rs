//! Japanese on screen: the katakana the decode effect scrambles through, and whether a font here
//! draws full-width CJK at all. The Chain screen's subtitles join this table when they are drawn.
//!
//! This is the one file whose literals may hold CJK: `glyphs_stay_in_the_nerd_font_set` lets
//! kana and kanji through here and nowhere else, so every CJK string is drawn through code that
//! knows it takes two cells a character, and only where [`renders`] says a font covers it.

/// Full-width katakana for the decode effect, all two cells wide.
pub const DECODE: [&str; 20] =
    ["ア", "カ", "サ", "タ", "ナ", "ハ", "マ", "ヤ", "ラ", "ワ", "イ", "キ", "シ", "チ", "ニ", "ヒ", "ミ", "リ", "ウ", "ク"];

/// Japanese beside the Chain screen's panel titles, by panel.
const SUBTITLES: [(&str, &str); 7] = [
    ("timer", "時計"),
    ("head", "先頭"),
    ("lattice", "格子"),
    ("entropy", "エントロピー"),
    ("hashrate", "採掘"),
    ("gas", "手数料"),
    ("blocks", "台帳"),
];

/// A panel title with its Japanese subtitle where a font draws it: "lattice 格子".
pub fn titled(key: &str, english: &str) -> String {
    match SUBTITLES.iter().find(|(k, _)| *k == key) {
        Some((_, jp)) if renders() => format!("{english} {jp}"),
        _ => english.to_string(),
    }
}

static CJK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Whether a font here draws full-width katakana and kanji. Never waits: until the probe has
/// answered (or where it can't, over SSH or on the Linux console) the answer is no, and callers
/// fall back to ASCII.
pub fn renders() -> bool {
    CJK.get().copied().unwrap_or(false)
}

/// Run by `fx::probe_fonts`, off the render path: one fontconfig query for a font holding both
/// full-width katakana (U+30A2) and a kanji (U+4E3B).
pub(crate) fn probe(local: bool) {
    let covered = local
        && (cfg!(target_os = "macos")
            || std::process::Command::new("fc-list")
                .args([":charset=30a2 4e3b", "family"])
                .stderr(std::process::Stdio::null())
                .output()
                .is_ok_and(|out| out.status.success() && !out.stdout.trim_ascii().is_empty()));
    let _ = CJK.set(covered);
}

/// The ranges this file may use: CJK punctuation, hiragana, katakana and unified ideographs.
#[cfg(test)]
pub(crate) fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3000..=0x303F | 0x3040..=0x309F | 0x30A0..=0x30FF | 0x4E00..=0x9FFF)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn every_subtitle_is_cjk_and_titles_fall_back_to_english() {
        for (_, jp) in SUBTITLES {
            assert!(jp.chars().all(is_cjk), "{jp}");
            assert_eq!(jp.width(), jp.chars().count() * 2, "{jp}");
        }
        // No font answer in tests: English alone.
        assert_eq!(titled("lattice", "lattice"), "lattice");
    }

    #[test]
    fn every_decode_glyph_is_one_cjk_character_two_cells_wide() {
        for k in DECODE {
            assert_eq!(k.chars().count(), 1, "{k}");
            assert!(k.chars().all(is_cjk), "{k}");
            assert_eq!(k.width(), 2, "{k}");
        }
    }
}
