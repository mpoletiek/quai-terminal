//! Exact decimal amount parsing and formatting. No floating point.

use crate::error::{CoreError, Result};
use quai_sdk::U256;

/// Exact floor of `a * b / divisor`, with a full-width intermediate. None means division by
/// zero or a result that cannot be represented in token atoms.
pub fn mul_div(a: U256, b: U256, divisor: U256) -> Option<U256> {
    if divisor.is_zero() {
        return None;
    }
    let result = a.widening_mul::<256, 4, 512, 8>(b) / divisor.widening_mul::<256, 4, 512, 8>(U256::from(1));
    let limbs = result.as_limbs();
    (result.bit_len() <= 256).then(|| U256::from_limbs([limbs[0], limbs[1], limbs[2], limbs[3]]))
}

/// A display ratio without first rounding a holding to whole basis points.
pub fn ratio(numerator: U256, denominator: U256) -> Option<f64> {
    if denominator.is_zero() {
        return None;
    }
    let value = to_f64(numerator, 0) / to_f64(denominator, 0);
    value.is_finite().then_some(value)
}
use quai_sdk::primitives::{Unit, format_units, parse_units};

/// Decimals for QUAI.
pub const QUAI_DECIMALS: u8 = 18;
/// Decimals for Qi (1 Qi = 1000 Qits).
pub const QI_DECIMALS: u8 = 3;

/// Parse a human decimal string with `decimals` into base units.
/// Rejects negatives, exponents, whitespace and excess precision.
pub fn parse_amount(text: &str, decimals: u8) -> Result<U256> {
    let trimmed = text.trim();
    if trimmed.is_empty()
        || trimmed != text
        || trimmed.starts_with('-')
        || trimmed.starts_with('+')
        || trimmed.contains(['e', 'E', '_', ','])
    {
        return Err(CoreError::Invalid(format!("invalid amount `{text}`")));
    }
    let unit = Unit::new(decimals).map_err(|e| CoreError::Invalid(format!("unit: {e}")))?;
    parse_units(trimmed, unit).map_err(|e| CoreError::Invalid(format!("invalid amount `{text}`: {e}")))
}

/// Format base units with `decimals`, trimming trailing zeros (at least one fractional digit kept off).
pub fn format_amount(value: U256, decimals: u8) -> String {
    let unit = Unit::new(decimals).unwrap_or(Unit::BASE);
    let text = format_units(value, unit);
    trim_fraction(&text)
}

/// Format with a fixed maximum number of fractional digits (truncating, never rounding up).
pub fn format_amount_short(value: U256, decimals: u8, max_fraction: usize) -> String {
    let full = format_amount(value, decimals);
    match full.split_once('.') {
        Some((whole, frac)) if frac.len() > max_fraction => {
            let cut = &frac[..max_fraction];
            let cut = cut.trim_end_matches('0');
            if cut.is_empty() {
                if value.is_zero() {
                    "0".into()
                } else if whole == "0" {
                    format!("<0.{}1", "0".repeat(max_fraction.saturating_sub(1)))
                } else {
                    whole.to_string()
                }
            } else {
                format!("{whole}.{cut}")
            }
        }
        _ => full,
    }
}

/// Group the integer part with thin separators for display (e.g. `1,204.5`).
pub fn group_thousands(text: &str) -> String {
    let (sign, rest) = text.strip_prefix('<').map_or(("", text), |r| ("<", r));
    let (whole, frac) = rest.split_once('.').map_or((rest, None), |(w, f)| (w, Some(f)));
    let mut out = String::new();
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    match frac {
        Some(f) => format!("{sign}{out}.{f}"),
        None => format!("{sign}{out}"),
    }
}

fn trim_fraction(text: &str) -> String {
    match text.split_once('.') {
        Some((whole, frac)) => {
            let frac = frac.trim_end_matches('0');
            if frac.is_empty() { whole.to_string() } else { format!("{whole}.{frac}") }
        }
        None => text.to_string(),
    }
}

/// Format QUAI base units.
pub fn quai(value: U256) -> String {
    format_amount(value, QUAI_DECIMALS)
}

/// Format Qits as Qi.
pub fn qi(value: U256) -> String {
    format_amount(value, QI_DECIMALS)
}

/// Parse a QUAI decimal string.
pub fn parse_quai(text: &str) -> Result<U256> {
    parse_amount(text, QUAI_DECIMALS)
}

/// Parse a Qi decimal string into Qits.
pub fn parse_qi(text: &str) -> Result<U256> {
    parse_amount(text, QI_DECIMALS)
}

/// Basis points of `part` relative to `whole`, saturating; None when whole is zero.
pub fn bps(part: U256, whole: U256) -> Option<u64> {
    if whole.is_zero() {
        return None;
    }
    let scaled = part.saturating_mul(U256::from(10_000u64)) / whole;
    Some(u64::try_from(scaled).unwrap_or(u64::MAX))
}

/// Parse an indexer integer that may use JavaScript exponent notation (`1e+22`,
/// `1.7645481533e+24`). Such values can be rounded by the indexer, so callers treat them as
/// approximate display data and re-read exact balances on-chain where it matters.
pub fn parse_indexer_integer(text: &str) -> Option<U256> {
    let t = text.trim();
    if t.is_empty() || t.starts_with('-') {
        return None;
    }
    let (mantissa, exponent) = match t.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.trim_start_matches('+').parse::<i64>().ok()?),
        None => (t, 0),
    };
    let (whole, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if !whole.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) || whole.len() + frac.len() == 0 {
        return None;
    }
    let digits = format!("{whole}{frac}");
    let shift = exponent - frac.len() as i64;
    let text = if shift >= 0 {
        if shift > 78 {
            return None;
        }
        format!("{digits}{}", "0".repeat(shift as usize))
    } else {
        // Negative shift drops fractional digits (indexer integers never carry real fractions).
        let keep = digits.len() as i64 + shift;
        if keep <= 0 { "0".to_string() } else { digits[..keep as usize].to_string() }
    };
    let text = text.trim_start_matches('0');
    if text.is_empty() {
        return Some(U256::ZERO);
    }
    U256::from_str_radix(text, 10).ok()
}

/// Approximate float of base units, for USD display only (never for amounts that are signed).
pub fn to_f64(value: U256, decimals: u8) -> f64 {
    format_amount(value, decimals).parse::<f64>().unwrap_or(0.0)
}

/// Compact token counts for figures too large to read in full: `18.3M`, `297.6k`, `842`.
/// Display only — never a figure anyone signs.
pub fn compact(value: f64) -> String {
    if !value.is_finite() {
        return "—".into();
    }
    let v = value.abs();
    let sign = if value < 0.0 { "-" } else { "" };
    if v >= 1e9 {
        format!("{sign}{:.1}B", v / 1e9)
    } else if v >= 1e6 {
        format!("{sign}{:.1}M", v / 1e6)
    } else if v >= 1e3 {
        format!("{sign}{:.1}k", v / 1e3)
    } else {
        format!("{sign}{}", group_thousands(&format!("{v:.0}")))
    }
}

/// A count and its noun, in the right number: `1 transaction`, `3 transactions`, `0 coins`.
/// `noun` is the singular; a noun that does not take a plain `s` passes its plural after a
/// `|` (`"sender|senders"` is the same as `"sender"`, `"entry|entries"` is not).
pub fn count<N: std::fmt::Display + PartialEq + From<u8>>(n: N, noun: &str) -> String {
    let (one, many) = match noun.split_once('|') {
        Some((one, many)) => (one.to_string(), many.to_string()),
        None => (noun.to_string(), format!("{noun}s")),
    };
    let one_of = n == N::from(1);
    format!("{} {}", group_thousands(&n.to_string()), if one_of { one } else { many })
}

/// USD display: `$1,284.52`, `$0.00881`, `<$0.01`.
pub fn usd(value: f64) -> String {
    if !value.is_finite() {
        return "—".into();
    }
    let sign = if value < 0.0 { "-" } else { "" };
    let v = value.abs();
    if v == 0.0 {
        return "$0.00".into();
    }
    if v < 0.01 {
        return format!("{sign}<$0.01");
    }
    let text = format!("{v:.2}");
    format!("{sign}${}", group_thousands(&text))
}

/// A value below one thousandth, in the notation DEX screens use: the zeros after the point are
/// counted in a subscript and only the significant digits are written, so `0.0000009835` is
/// `0.0₆9835`. It stays the same width however small the price gets, and it cannot be misread by
/// a zero — which is what a column of `0.0000001274`, `0.00000001274` invites. `None` at or above
/// 0.001, where plain decimals are already short.
pub fn subscript_zeros(value: f64, significant: usize) -> Option<String> {
    if !value.is_finite() || value <= 0.0 || value >= 0.001 {
        return None;
    }
    const SUB: [char; 10] = ['₀', '₁', '₂', '₃', '₄', '₅', '₆', '₇', '₈', '₉'];
    // Round to the significant digits first, so 0.00009999 becomes 0.0001 rather than 0.0₄9999·
    let text = format!("{value:.*e}", significant.saturating_sub(1));
    let (mantissa, exponent) = text.split_once('e')?;
    let exponent: i32 = exponent.parse().ok()?;
    let zeros = (-exponent - 1) as usize;
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    if zeros < 3 {
        return Some(format!("0.{}{digits}", "0".repeat(zeros)));
    }
    let count: String = zeros.to_string().chars().map(|c| SUB[c.to_digit(10).unwrap_or(0) as usize]).collect();
    Some(format!("0.0{count}{digits}"))
}

/// Unit price display with enough significant digits for sub-cent tokens: `$0.008841`, `$1.0492`,
/// and `$0.0₆9835` below a thousandth (see [`subscript_zeros`]).
pub fn usd_price(value: f64) -> String {
    if !value.is_finite() || value <= 0.0 {
        return "—".into();
    }
    if let Some(tiny) = subscript_zeros(value, 4) {
        return format!("${tiny}");
    }
    if value >= 1000.0 {
        format!("${}", group_thousands(&format!("{value:.2}")))
    } else if value >= 1.0 {
        format!("${value:.4}")
    } else {
        let digits = (-value.log10()).floor() as usize + 4;
        format!("${:.*}", digits.min(12), value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_agree_with_their_nouns() {
        assert_eq!(count(1, "transaction"), "1 transaction");
        assert_eq!(count(0, "coin"), "0 coins");
        assert_eq!(count(1234, "message"), "1,234 messages");
        assert_eq!(count(2, "entry|entries"), "2 entries");
        assert_eq!(count(1, "entry|entries"), "1 entry");
    }

    #[test]
    fn tiny_values_count_their_zeros() {
        assert_eq!(subscript_zeros(0.0000009835, 4).as_deref(), Some("0.0₆9835"));
        assert_eq!(subscript_zeros(0.0000001274, 4).as_deref(), Some("0.0₆1274"));
        assert_eq!(subscript_zeros(0.00000000001234, 4).as_deref(), Some("0.0₁₀1234"), "two-digit counts");
        assert_eq!(subscript_zeros(0.0001493, 4).as_deref(), Some("0.0₃1493"));
        assert_eq!(subscript_zeros(0.0005, 4).as_deref(), Some("0.0₃5"), "no trailing zeros");
        assert_eq!(subscript_zeros(0.00009999, 2).as_deref(), Some("0.0₃1"), "rounding carries into the next place");
        assert_eq!(subscript_zeros(0.001, 4), None);
        assert_eq!(subscript_zeros(0.0, 4), None);
        assert_eq!(subscript_zeros(f64::NAN, 4), None);
        assert_eq!(usd_price(0.0000009835), "$0.0₆9835");
        assert_eq!(usd_price(0.008841), "$0.008841", "a thousandth and up is plain");
    }

    #[test]
    fn parse_and_format_exact() {
        assert_eq!(parse_quai("1").unwrap(), U256::from(10u128.pow(18)));
        assert_eq!(parse_quai("0.000000000000000001").unwrap(), U256::from(1));
        assert_eq!(parse_qi("1.5").unwrap(), U256::from(1500));
        assert!(parse_qi("0.0001").is_err());
        assert!(parse_quai("-1").is_err());
        assert!(parse_quai("1e3").is_err());
        assert!(parse_quai(" 1").is_err());
        assert!(parse_quai("").is_err());
        assert!(parse_quai("1,000").is_err());
        assert_eq!(quai(U256::from(15u128 * 10u128.pow(17))), "1.5");
        assert_eq!(qi(U256::from(1000)), "1");
        assert_eq!(qi(U256::from(1)), "0.001");
        assert_eq!(quai(U256::ZERO), "0");
    }

    #[test]
    fn short_and_grouped() {
        let v = parse_quai("1204.518300123").unwrap();
        assert_eq!(format_amount_short(v, 18, 4), "1204.5183");
        assert_eq!(group_thousands("1204.5183"), "1,204.5183");
        assert_eq!(group_thousands("100"), "100");
        assert_eq!(group_thousands("1000000"), "1,000,000");
        assert_eq!(format_amount_short(U256::from(1), 18, 4), "<0.0001");
        assert_eq!(format_amount_short(U256::ZERO, 18, 4), "0");
    }

    #[test]
    fn indexer_integers_and_usd() {
        assert_eq!(parse_indexer_integer("1e+22"), Some(U256::from(10u128.pow(22))));
        assert_eq!(parse_indexer_integer("1.7645481533e+24"), Some(U256::from(17_645_481_533u128 * 10u128.pow(14))));
        assert_eq!(parse_indexer_integer("89236368082793"), Some(U256::from(89_236_368_082_793u64)));
        assert_eq!(parse_indexer_integer("20"), Some(U256::from(20)));
        assert_eq!(parse_indexer_integer("0"), Some(U256::ZERO));
        assert_eq!(parse_indexer_integer("-5"), None);
        assert_eq!(parse_indexer_integer("abc"), None);
        assert_eq!(parse_indexer_integer("1e+99"), None);
        assert_eq!(usd(1284.523), "$1,284.52");
        assert_eq!(usd(0.004), "<$0.01");
        assert_eq!(usd(0.0), "$0.00");
        assert_eq!(usd_price(0.008841), "$0.008841");
        assert_eq!(usd_price(1.0492), "$1.0492");
        assert_eq!(usd_price(0.0), "—");
        assert!((to_f64(parse_quai("12.5").unwrap(), 18) - 12.5).abs() < 1e-9);
    }

    #[test]
    fn basis_points() {
        assert_eq!(bps(U256::from(8), U256::from(1000)), Some(80));
        assert_eq!(bps(U256::from(1), U256::ZERO), None);
    }
}

#[cfg(test)]
mod trading_precision_regressions {
    use super::*;

    #[test]
    fn full_width_products_preserve_representable_quotients() {
        assert_eq!(mul_div(U256::MAX, U256::from(10_000), U256::from(10_000)), Some(U256::MAX));
        assert_eq!(mul_div(U256::MAX, U256::MAX, U256::MAX), Some(U256::MAX));
        assert_eq!(mul_div(U256::MAX, U256::from(2), U256::from(1)), None);
        assert_eq!(mul_div(U256::from(1), U256::from(1), U256::ZERO), None);
        assert_eq!(mul_div(U256::from(7), U256::from(3), U256::from(2)), Some(U256::from(10)));
    }
}
