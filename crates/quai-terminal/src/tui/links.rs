//! Hyperlinks (OSC 8) on the transaction hashes and addresses a frame shows.
//!
//! Every link goes to the configured explorer, built by the wallet from a hex id it already holds
//! in its own records: its accounts, its operations, its activity, its contacts, and the contracts
//! of the tokens it holds and the markets it lists. Nothing a collection, a token or a message says
//! ever becomes a link target — text elsewhere that happens to contain one of those ids links to
//! the same explorer page the wallet would open.
//!
//! The finished frame is scanned rather than each view marking its links, so a hash shown whole,
//! in groups of four (`0x 00F4 1a2B …`) or shortened (`0x00F4…804B`) is found wherever it is,
//! and the link covers exactly the cells that show it.

use super::app::App;
use ratatui::buffer::Buffer;

/// One run of cells that opens `url`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub y: u16,
    /// First cell, and one past the last.
    pub x: u16,
    pub end: u16,
    pub url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Tx,
    Address,
}

/// The ids this wallet holds, lowercase, without `0x`. Rebuilt when its records change.
#[derive(Default)]
pub struct Known {
    key: (usize, usize, usize, usize, usize, usize, String),
    ids: Vec<(String, Kind)>,
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn id(s: &str) -> Option<String> {
    let body = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    ((body.len() == 40 || body.len() == 64) && is_hex(body)).then(|| body.to_ascii_lowercase())
}

impl Known {
    fn refresh(&mut self, app: &App) {
        let d = &app.dash;
        let pools = match app.eco.markets_view.pools.shown() {
            Some(Ok((pools, _))) => pools.as_slice(),
            _ => &[],
        };
        let held = app.eco.feeds.portfolio.value().map(|p| p.rows.as_slice()).unwrap_or_default();
        let key = (d.accounts.len(), d.ops.len(), d.activity.len(), d.contacts.len(), pools.len(), held.len(), app.network_id.clone());
        if key == self.key {
            return;
        }
        let mut ids = Vec::new();
        let mut add = |s: &str, kind: Kind| {
            if let Some(h) = id(s) {
                ids.push((h, kind));
            }
        };
        for a in &d.accounts {
            add(&a.address, Kind::Address);
        }
        for o in &d.ops {
            if let Some(h) = &o.tx_hash {
                add(h, Kind::Tx);
            }
            add(&o.counterparty, Kind::Address);
        }
        for a in &d.activity {
            if let Some(h) = &a.tx_hash {
                add(h, Kind::Tx);
            }
            add(&a.address, Kind::Address);
        }
        for c in &d.contacts {
            if let Some(a) = &c.address {
                add(a, Kind::Address);
            }
        }
        // Market and token contracts, as the directory and the portfolio read them from the chain
        // and the explorer: the token info a market shows links to the page for what it names.
        for p in pools {
            add(&p.address, Kind::Address);
            add(&p.token0.address, Kind::Address);
            add(&p.token1.address, Kind::Address);
        }
        for r in held {
            if let wallet_core::portfolio::AssetKey::Token(contract) = &r.key {
                add(contract, Kind::Address);
            }
        }
        ids.sort();
        ids.dedup();
        self.ids = ids;
        self.key = key;
    }

    /// The one id that starts with `head` and ends with `tail` (hex, lowercase), if exactly one.
    fn resolve(&self, head: &str, tail: &str, whole: bool) -> Option<&(String, Kind)> {
        let mut found = self.ids.iter().filter(|(h, _)| h.starts_with(head) && h.ends_with(tail) && (!whole || h.len() == head.len()));
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }
}

/// A URL safe to put inside OSC 8: http(s), printable ASCII only, so it can neither end the
/// sequence early nor carry anything the terminal would act on.
pub(crate) fn safe(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://")) && url.len() < 2048 && url.bytes().all(|b| (0x21..0x7f).contains(&b))
}

/// Each row as its cells' symbols, so matches map back to columns.
fn row(buf: &Buffer, y: u16) -> Vec<&str> {
    (buf.area.x..buf.area.right()).map(|x| buf.cell((x, y)).map_or(" ", |c| c.symbol())).collect()
}

/// How many hex cells start at `i`.
fn hex_run(cells: &[&str], i: usize) -> usize {
    cells[i..].iter().take_while(|s| s.len() == 1 && s.as_bytes()[0].is_ascii_hexdigit()).count()
}

/// One candidate: where it is, and the hex it shows (head, and tail when shortened).
struct Seen {
    x: usize,
    end: usize,
    head: String,
    tail: String,
    whole: bool,
}

/// Every `0x…` on a row: whole, in groups of four, or shortened with `…`.
fn candidates(cells: &[&str]) -> Vec<Seen> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < cells.len() {
        if !(cells[i] == "0" && cells[i + 1] == "x") || (i > 0 && hex_run(cells, i - 1) > 0 && cells[i - 1] != " ") {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i + 2;
        let run = hex_run(cells, j);
        if run > 0 {
            let head: String = cells[j..j + run].concat();
            j += run;
            if cells.get(j) == Some(&"…") {
                let tail_run = hex_run(cells, j + 1);
                if run >= 4 && tail_run >= 4 {
                    let tail: String = cells[j + 1..j + 1 + tail_run].concat();
                    out.push(Seen { x: start, end: j + 1 + tail_run, head, tail, whole: false });
                    i = j + 1 + tail_run;
                    continue;
                }
            } else if run == 40 || run == 64 {
                out.push(Seen { x: start, end: j, head, tail: String::new(), whole: true });
            }
            i = j;
            continue;
        }
        // `0x` then groups of four hex digits, one space apart.
        let mut head = String::new();
        while cells.get(j) == Some(&" ")
            && hex_run(cells, j + 1) == 4
            && !cells.get(j + 5).is_some_and(|s| s.len() == 1 && s.as_bytes()[0].is_ascii_hexdigit())
        {
            head.push_str(&cells[j + 1..j + 5].concat());
            j += 5;
        }
        if head.len() >= 16 {
            let whole = head.len() == 40;
            out.push(Seen { x: start, end: j, head, tail: String::new(), whole });
        }
        i = j.max(i + 2);
    }
    out
}

/// The links in a finished frame.
pub fn scan(app: &App, buf: &Buffer) -> Vec<Link> {
    if !app.term.caps.hyperlinks || app.term.plain {
        return Vec::new();
    }
    let Some(net) = app.net() else { return Vec::new() };
    let mut known = app.term.links_known.borrow_mut();
    known.refresh(app);
    if known.ids.is_empty() {
        return Vec::new();
    }
    let mut links = Vec::new();
    for y in buf.area.y..buf.area.bottom() {
        let cells = row(buf, y);
        if !cells.windows(2).any(|w| w[0] == "0" && w[1] == "x") {
            continue;
        }
        for seen in candidates(&cells) {
            let (head, tail) = (seen.head.to_ascii_lowercase(), seen.tail.to_ascii_lowercase());
            let Some((hex, kind)) = known.resolve(&head, &tail, seen.whole) else { continue };
            let full = format!("0x{hex}");
            let url = match kind {
                Kind::Tx => net.tx_url(&full),
                Kind::Address => net.address_url(&full),
            };
            if let Some(url) = url.filter(|u| safe(u)) {
                links.push(Link { y, x: buf.area.x + seen.x as u16, end: buf.area.x + seen.end as u16, url });
            }
        }
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(s: &str) -> Vec<String> {
        s.chars().map(|c| c.to_string()).collect()
    }

    #[test]
    fn hashes_are_found_whole_grouped_and_shortened() {
        let a = "0x00F41a2B3c4D5e6F7a8B9c0D1e2F3a4B5c6D804B";
        let line = format!("to {a} · 0x 00F4 1a2B 3c4D 5e6F 7a8B 9c0D 1e2F 3a4B 5c6D 804B · 0x00F4…804B · 0x12 · x0x");
        let owned = cells(&line);
        let row: Vec<&str> = owned.iter().map(String::as_str).collect();
        let found = candidates(&row);
        assert_eq!(found.len(), 3, "whole, grouped, shortened; not a short hex or a stray 0x");
        assert!(found[0].whole && found[0].head.len() == 40 && found[0].end - found[0].x == 42);
        assert!(found[1].whole && found[1].head.eq_ignore_ascii_case(&a[2..]), "groups read as one id");
        assert_eq!((found[2].head.as_str(), found[2].tail.as_str(), found[2].whole), ("00F4", "804B", false));
        assert_eq!(&line.chars().skip(found[2].x).take(found[2].end - found[2].x).collect::<String>(), "0x00F4…804B");
    }

    #[test]
    fn only_one_known_id_resolves_a_shortened_one() {
        let known = Known {
            ids: vec![
                ("00f4aaaa804b".to_string() + &"0".repeat(28), Kind::Address),
                ("00f4bbbb".to_string() + &"1".repeat(32), Kind::Address),
            ],
            ..Default::default()
        };
        assert!(known.resolve("00f4", "0000", false).is_some(), "one ends that way");
        assert!(known.resolve("00f4", "", false).is_none(), "two start that way: ambiguous, no link");
        assert!(known.resolve("dead", "", false).is_none(), "not the wallet's: no link");
    }

    #[test]
    fn a_url_cannot_end_the_sequence() {
        assert!(safe("https://quaiscan.io/tx/0xabc"));
        assert!(!safe("https://evil\x1b\\x"), "no escape");
        assert!(!safe("https://a b"), "no space");
        assert!(!safe("javascript:alert(1)"), "http(s) only");
    }
}
