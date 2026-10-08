//! Digits drawn with cells, for numbers that are read across a room: the balance hero's three-row
//! block digits, five-row block digits, and a seven-segment face for instruments (timers,
//! counters). Every glyph here is in the Nerd Font set (`ui_tests::GLYPHS`).

// The five-row and seven-segment faces are for the Chain screen (docs/VISUAL_PLAN_2026-10-08.md S1).
#![cfg_attr(not(test), allow(dead_code))]

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

const FIVE: [[&str; 5]; 10] = [
    ["███", "█ █", "█ █", "█ █", "███"],
    [" █ ", "██ ", " █ ", " █ ", "███"],
    ["███", "  █", "███", "█  ", "███"],
    ["███", "  █", " ██", "  █", "███"],
    ["█ █", "█ █", "███", "  █", "  █"],
    ["███", "█  ", "███", "  █", "███"],
    ["███", "█  ", "███", "█ █", "███"],
    ["███", "  █", "  █", "  █", "  █"],
    ["███", "█ █", "███", "█ █", "███"],
    ["███", "█ █", "███", "  █", "███"],
];

/// Five-row block digits, one column between glyphs: digits, `,` (a stroke from the baseline, as
/// in `big_digits`), `.` and spaces. Anything else is a blank column.
pub fn five_row(text: &str) -> [String; 5] {
    let mut rows: [String; 5] = Default::default();
    for (i, c) in text.chars().enumerate() {
        if i > 0 {
            rows.iter_mut().for_each(|r| r.push(' '));
        }
        match c {
            d @ '0'..='9' => {
                for (r, part) in rows.iter_mut().zip(FIVE[d as usize - '0' as usize]) {
                    r.push_str(part);
                }
            }
            ',' => rows.iter_mut().enumerate().for_each(|(i, r)| r.push(if i == 4 { '▌' } else { ' ' })),
            '.' => rows.iter_mut().enumerate().for_each(|(i, r)| r.push(if i == 4 { '▄' } else { ' ' })),
            _ => rows.iter_mut().for_each(|r| r.push(' ')),
        }
    }
    rows
}

/// Seven-segment digits, four cells wide and five rows tall, for instruments: digits, `.` and `:`
/// (one cell each), `-` (the middle bar) and spaces. One column between glyphs.
pub fn seven_seg(text: &str) -> [String; 5] {
    // Segments a b c d e f g: top, upper right, lower right, bottom, lower left, upper left, middle.
    const SEG: [[bool; 7]; 10] = [
        [true, true, true, true, true, true, false],
        [false, true, true, false, false, false, false],
        [true, true, false, true, true, false, true],
        [true, true, true, true, false, false, true],
        [false, true, true, false, false, true, true],
        [true, false, true, true, false, true, true],
        [true, false, true, true, true, true, true],
        [true, true, true, false, false, false, false],
        [true, true, true, true, true, true, true],
        [true, true, true, true, false, true, true],
    ];
    let bar = |on: bool| if on { " ━━ " } else { "    " };
    let sides = |l: bool, r: bool| format!("{}  {}", if l { '┃' } else { ' ' }, if r { '┃' } else { ' ' });
    let mut rows: [String; 5] = Default::default();
    for (i, c) in text.chars().enumerate() {
        if i > 0 {
            rows.iter_mut().for_each(|r| r.push(' '));
        }
        match c {
            d @ '0'..='9' => {
                let g = SEG[d as usize - '0' as usize];
                rows[0].push_str(bar(g[0]));
                rows[1].push_str(&sides(g[5], g[1]));
                rows[2].push_str(bar(g[6]));
                rows[3].push_str(&sides(g[4], g[2]));
                rows[4].push_str(bar(g[3]));
            }
            '-' => {
                for (i, r) in rows.iter_mut().enumerate() {
                    r.push_str(bar(i == 2));
                }
            }
            '.' => rows.iter_mut().enumerate().for_each(|(i, r)| r.push(if i == 4 { '▪' } else { ' ' })),
            ':' => rows.iter_mut().enumerate().for_each(|(i, r)| r.push(if i == 1 || i == 3 { '▪' } else { ' ' })),
            _ => rows.iter_mut().for_each(|r| r.push(' ')),
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    fn even(rows: &[String]) -> bool {
        rows.iter().all(|r| r.width() == rows[0].width())
    }

    #[test]
    fn every_face_keeps_its_rows_even() {
        for text in ["0123456789", "10,390,405", "04.1", "12:05", "-1", " 7 "] {
            assert!(even(&big_digits(text)), "big_digits {text}");
            assert!(even(&five_row(text)), "five_row {text}");
            assert!(even(&seven_seg(text)), "seven_seg {text}");
        }
    }

    #[test]
    fn widths_are_what_layouts_reserve() {
        // A digit is 3 cells in the block faces and 4 in seven segments; one column between glyphs.
        assert_eq!(five_row("88")[0].width(), 7);
        assert_eq!(seven_seg("88")[0].width(), 9);
        assert_eq!(seven_seg("04.1")[0].width(), 4 + 1 + 4 + 1 + 1 + 1 + 4);
    }

    #[test]
    fn seven_segments_draw_an_eight_whole_and_a_one_thin() {
        assert_eq!(seven_seg("8"), [" ━━ ", "┃  ┃", " ━━ ", "┃  ┃", " ━━ "].map(String::from));
        assert_eq!(seven_seg("1"), ["    ", "   ┃", "    ", "   ┃", "    "].map(String::from));
    }
}
