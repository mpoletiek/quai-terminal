//! Numbers as the screen writes them.
//!
//! The formatting itself lives in `wallet_core::amount` (grouping, USD, subscript zeros), shared
//! with the CLI. This module adds what only a screen needs: the true minus sign, signed
//! percentages, and columns whose decimal points line up. The CLI keeps ASCII `-` so its output
//! still parses.

/// U+2212, the minus sign: as wide as `+` and set at the same height, where a hyphen is short
/// and low and reads as a dash.
pub const MINUS: char = '\u{2212}';

/// A display number with its leading hyphen-minus (after any `$` or `<`) turned into the true
/// minus sign. Anything else is left alone.
pub fn minus(text: impl Into<String>) -> String {
    let text = text.into();
    match text.find('-') {
        Some(i) if text[..i].chars().all(|c| matches!(c, '$' | '<' | ' ')) => {
            format!("{}{MINUS}{}", &text[..i], &text[i + 1..])
        }
        _ => text,
    }
}

use wallet_core::amount;
use wallet_core::sdk::U256;

/// Qits as Qi, grouped: `12,500.25`.
pub fn qi(qits: U256) -> String {
    amount::group_thousands(&amount::qi(qits))
}

/// Base units at `decimals`, at most `fraction` digits after the point, grouped: `179,071.7533`.
pub fn short(value: U256, decimals: u8, fraction: usize) -> String {
    amount::group_thousands(&amount::format_amount_short(value, decimals, fraction))
}

/// An amount being typed, grouped as it will be shown once entered: `12500.5` reads
/// `12,500.5`, and a trailing point stays (`1000.` → `1,000.`). Anything that is not plain digits
/// is shown exactly as typed. Display only: the field keeps what was typed.
pub fn typed(text: &str) -> String {
    let (whole, frac) = text.split_once('.').map_or((text, None), |(w, f)| (w, Some(f)));
    if whole.is_empty() || !whole.chars().all(|c| c.is_ascii_digit()) || !frac.is_none_or(|f| f.chars().all(|c| c.is_ascii_digit())) {
        return text.to_string();
    }
    let grouped = wallet_core::amount::group_thousands(whole);
    match frac {
        Some(f) => format!("{grouped}.{f}"),
        None => grouped,
    }
}

/// A unit as it is written: `Qi` (never `QI`), `QUAI`, anything else as given.
pub fn unit(asset: &str) -> &str {
    if asset.eq_ignore_ascii_case("qi") {
        "Qi"
    } else if asset.eq_ignore_ascii_case("quai") {
        "QUAI"
    } else {
        asset
    }
}

/// A change in percent, always signed: `+4.1%`, `−3.2%`, `0.0%`.
pub fn pct(change: f64, decimals: usize) -> String {
    if !change.is_finite() {
        return "—".into();
    }
    let text = format!("{:.*}", decimals, change.abs());
    // A change that rounds to zero has no direction.
    if text.chars().all(|c| c == '0' || c == '.') {
        return format!("{text}%");
    }
    format!("{}{text}%", if change > 0.0 { '+' } else { MINUS })
}

/// Right-aligned numbers whose decimal points line up: each cell is padded after its last digit
/// to the longest fraction in the column, so `1,259.322` sits over `12.5  ` and `400    `.
/// Whatever follows the number (a unit, a `~`) keeps its place after the padding. Cells with no
/// digits (`—`) are left as they are.
pub fn align(cells: &[String]) -> Vec<String> {
    let split = |c: &str| -> Option<(usize, usize)> {
        // (end of the number, digits after its point)
        let end = c.char_indices().rfind(|(_, ch)| ch.is_ascii_digit()).map(|(i, _)| i + 1)?;
        let frac = c[..end].rfind('.').map_or(0, |p| c[p + 1..end].chars().count());
        Some((end, frac))
    };
    let widest = cells.iter().filter_map(|c| split(c)).map(|(_, f)| f).max().unwrap_or(0);
    cells
        .iter()
        .map(|c| match split(c) {
            Some((end, frac)) => {
                // A whole number gets room for the point as well as the digits.
                let pad = if frac == 0 && widest > 0 && !c[..end].contains('.') { widest + 1 } else { widest - frac };
                format!("{}{}{}", &c[..end], " ".repeat(pad), &c[end..])
            }
            None => c.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_numbers_take_the_minus_sign() {
        assert_eq!(minus("-12.50"), "−12.50");
        assert_eq!(minus("$-3.10"), "$−3.10");
        assert_eq!(minus("12-14"), "12-14", "a range is not a sign");
        assert_eq!(minus("+4.00"), "+4.00");
    }

    #[test]
    fn typed_amounts_are_grouped() {
        assert_eq!(typed("12500.5"), "12,500.5");
        assert_eq!(typed("1000."), "1,000.");
        assert_eq!(typed("999"), "999");
        assert_eq!(typed(".5"), ".5");
        assert_eq!(typed("max"), "max");
        assert_eq!(typed("0x12ab"), "0x12ab");
    }

    #[test]
    fn units_are_spelled_as_the_network_spells_them() {
        assert_eq!(unit("QI"), "Qi");
        assert_eq!(unit("quai"), "QUAI");
        assert_eq!(unit("WQI"), "WQI");
    }

    #[test]
    fn percentages_are_signed_and_zero_is_not() {
        assert_eq!(pct(4.12, 1), "+4.1%");
        assert_eq!(pct(-3.25, 1), "−3.2%");
        assert_eq!(pct(-0.01, 1), "0.0%");
        assert_eq!(pct(f64::NAN, 1), "—");
    }

    #[test]
    fn decimal_points_line_up() {
        // Suffixes are the same width in every cell ("  " where there is no `~`).
        let col = align(&["1,259.322  ".into(), "12.5 ~".into(), "400  ".into(), "—".into()]);
        assert_eq!(col, vec!["1,259.322  ", "12.5   ~", "400      ", "—"]);
        // Right-aligned, the points (and where 400's would be) share a column.
        let w = col.iter().map(|c| c.chars().count()).max().unwrap();
        let point = |c: &str, at: usize| w - c.chars().count() + at;
        assert_eq!(point(&col[0], 5), point(&col[1], 2));
        assert_eq!(point(&col[0], 5), point(&col[2], 3));
    }
}
