//! Human and JSON output.

use crate::args::Output;
use serde::Serialize;
use wallet_core::CoreError;
use wallet_core::tx::Review;

/// Output context.
#[derive(Clone, Copy)]
pub struct Out {
    pub format: Output,
    pub color: bool,
}

const RESET: &str = "\x1b[0m";

impl Out {
    pub fn json(&self) -> bool {
        self.format == Output::Json
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.color { format!("\x1b[{code}m{text}{RESET}") } else { text.to_string() }
    }
    pub fn bold(&self, t: &str) -> String {
        self.paint("1", t)
    }
    pub fn dim(&self, t: &str) -> String {
        self.paint("2", t)
    }
    pub fn green(&self, t: &str) -> String {
        self.paint("32", t)
    }
    pub fn yellow(&self, t: &str) -> String {
        self.paint("33", t)
    }
    pub fn red(&self, t: &str) -> String {
        self.paint("31", t)
    }
    pub fn cyan(&self, t: &str) -> String {
        self.paint("36", t)
    }
    pub fn magenta(&self, t: &str) -> String {
        self.paint("35", t)
    }

    /// Emit a JSON envelope for a successful command.
    pub fn emit<T: Serialize>(&self, command: &str, result: &T) {
        let envelope = serde_json::json!({
            "schema_version": 1,
            "command": command,
            "ok": true,
            "result": result,
        });
        println!("{}", serde_json::to_string_pretty(&envelope).unwrap_or_default());
    }

    /// Report an error in the selected format.
    pub fn error(&self, command: &str, err: &CoreError) {
        if self.json() {
            let envelope = serde_json::json!({
                "schema_version": 1,
                "command": command,
                "ok": false,
                "error": {"kind": err.kind(), "message": err.to_string(), "exit_code": err.exit_code()},
            });
            println!("{}", serde_json::to_string_pretty(&envelope).unwrap_or_default());
        } else {
            eprintln!("{} {}", self.red("error:"), wallet_core::explorer::clean(&err.to_string(), usize::MAX));
        }
    }

    /// Print a review to stderr (keeps stdout clean for JSON).
    pub fn review(&self, r: &Review) {
        // Preserve full financial values while removing terminal controls and deceptive formatting.
        let mut safe = r.clone();
        let clean = |text: &mut String| *text = wallet_core::explorer::clean(text, usize::MAX);
        for text in [
            &mut safe.title,
            &mut safe.network,
            &mut safe.from,
            &mut safe.to,
            &mut safe.asset,
            &mut safe.amount,
            &mut safe.max_fee,
            &mut safe.op_id,
        ] {
            clean(text);
        }
        for field in &mut safe.fields {
            clean(&mut field.label);
            clean(&mut field.value);
        }
        for change in &mut safe.changes {
            clean(&mut change.asset);
            clean(&mut change.amount);
            clean(&mut change.note);
        }
        for coin in &mut safe.coins {
            clean(&mut coin.address);
            clean(&mut coin.role);
        }
        for warning in &mut safe.warnings {
            clean(warning);
        }
        let r = &safe;
        let w = &mut std::io::stderr();
        use std::io::Write;
        let _ = writeln!(w);
        let _ = writeln!(w, "{} {}", self.bold("┌ review ·"), self.bold(&r.title));
        // What it does to the balances, before the detail.
        let amount_w = r.changes.iter().map(|c| c.amount.chars().count()).max().unwrap_or(0);
        for c in &r.changes {
            let sign = match c.direction.as_str() {
                "in" => "+",
                "none" => "·",
                _ => "−",
            };
            let line = if c.direction == "none" {
                format!("│ {sign} {}", c.note)
            } else {
                format!("│ {sign} {:>amount_w$} {}  {}", c.amount, c.asset, self.dim(&c.note))
            };
            let _ = writeln!(w, "{line}");
        }
        if !r.changes.is_empty() {
            let _ = writeln!(w, "│");
        }
        let row = |label: &str, value: &str| format!("│ {:<16} {}", label, value);
        let _ = writeln!(w, "{}", row("network", &r.network));
        let _ = writeln!(w, "{}", row("from", &r.from));
        let _ = writeln!(w, "{}", row("to", &r.to));
        let _ = writeln!(w, "{}", row("amount", &self.bold(&r.amount)));
        let fee = match r.fee_bps {
            Some(bps) => format!("≤ {}  ({:.2}% of amount)", r.max_fee, bps as f64 / 100.0),
            None => format!("≤ {}", r.max_fee),
        };
        let _ = writeln!(w, "{}", row("max fee", &fee));
        for f in &r.fields {
            let _ = writeln!(w, "{}", row(&f.label.to_lowercase(), &f.value));
        }
        if !r.coins.is_empty() {
            let _ = writeln!(w, "│ {}", self.dim("coins"));
            for c in &r.coins {
                let _ = writeln!(
                    w,
                    "│   {:<9} {:>12} Qi  {}",
                    c.role,
                    wallet_core::amount::qi(wallet_core::sdk::U256::from(c.qits)),
                    c.address
                );
            }
        }
        for warning in &r.warnings {
            let _ = writeln!(w, "│ {} {}", self.yellow("!"), self.yellow(warning));
        }
        let _ = writeln!(w, "└ operation {}", self.dim(&r.op_id));
    }

    /// Print a simple table.
    pub fn table(&self, headers: &[&str], rows: &[Vec<String>]) {
        let safe_rows: Vec<Vec<String>> =
            rows.iter().map(|row| row.iter().map(|cell| wallet_core::explorer::clean(cell, usize::MAX)).collect()).collect();
        let rows = safe_rows.as_slice();
        let mut widths: Vec<usize> = headers.iter().map(|h| unicode_width::UnicodeWidthStr::width(*h)).collect();
        for row in rows {
            for (i, cell) in row.iter().enumerate() {
                if i < widths.len() {
                    widths[i] = widths[i].max(unicode_width::UnicodeWidthStr::width(cell.as_str()));
                }
            }
        }
        let fmt_row = |cells: Vec<String>| {
            cells
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let pad = widths[i].saturating_sub(unicode_width::UnicodeWidthStr::width(c.as_str()));
                    format!("{c}{}", " ".repeat(pad))
                })
                .collect::<Vec<_>>()
                .join("  ")
        };
        println!("{}", self.dim(&fmt_row(headers.iter().map(|h| h.to_string()).collect())));
        for row in rows {
            println!("{}", fmt_row(row.clone()));
        }
    }
}

/// Render a QR code with Unicode half blocks for terminals.
pub fn qr_text(data: &str) -> String {
    use qrcode::{Color, EcLevel, QrCode};
    let Ok(code) = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::M) else {
        return String::from("(QR unavailable)");
    };
    let width = code.width();
    let colors = code.to_colors();
    let quiet = 2usize;
    let size = width + quiet * 2;
    let dark = |x: usize, y: usize| -> bool {
        if x < quiet || y < quiet || x >= width + quiet || y >= width + quiet {
            return false;
        }
        colors[(y - quiet) * width + (x - quiet)] == Color::Dark
    };
    let mut out = String::new();
    let mut y = 0;
    while y < size {
        for x in 0..size {
            let top = dark(x, y);
            let bottom = y + 1 < size && dark(x, y + 1);
            // Light background, dark modules: print inverted blocks for scanner contrast.
            out.push(match (top, bottom) {
                (true, true) => ' ',
                (true, false) => '▄',
                (false, true) => '▀',
                (false, false) => '█',
            });
        }
        out.push('\n');
        y += 2;
    }
    out
}
