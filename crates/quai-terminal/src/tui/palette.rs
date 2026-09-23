//! The command palette: one fuzzy search over everything the wallet can do or show.
//!
//! It reads a typed intent first ("send alice 5 quai", "swap 10 wqi to usdt"), then offers what
//! was used recently, then every action, screen, contact, holding and market that matches. An
//! entry never signs anything: sends open a filled form and swaps a filled card, and the review
//! still decides.

use super::app::{ACTIONS, Action, App, FormKind, Modal, Screen};
use wallet_core::config::Feature;
use wallet_core::swap::SwapAsset;

/// What choosing an entry does.
#[derive(Clone, Debug, PartialEq)]
pub enum Run {
    Action(&'static str),
    Go(Screen),
    /// A send form with what the query said filled in.
    Send {
        kind: FormKind,
        to: String,
        amount: String,
        token: Option<String>,
    },
    /// The swap card with its pair (and amount) set.
    Swap {
        from: SwapAsset,
        to: Option<SwapAsset>,
        amount: String,
    },
    /// Markets, on this pool.
    Market(String),
    /// The glossary, open at this term.
    Term(usize),
    /// Remove an alert.
    RemoveAlert(u64),
    /// Write to the pinned chat.
    WritePinned,
    /// Pin a chat (`None` unpins), with how it reads.
    Pin(Option<String>, String),
    /// Subscribe to a chat's messages, or stop.
    Subscribe(String, String),
}

/// One row of the palette.
#[derive(Clone, Debug)]
pub struct Entry {
    /// `do`, `recent`, `action`, `go`, `contact`, `asset` or `market`.
    pub tag: &'static str,
    pub label: String,
    /// Key that does the same without the palette, or a short detail.
    pub hint: String,
    /// The equivalent CLI command, when there is one.
    pub cli: String,
    pub run: Run,
}

impl Entry {
    /// Stable identity for the recents list.
    pub fn key(&self) -> String {
        match &self.run {
            Run::Action(id) => format!("action:{id}"),
            Run::Go(s) => format!("go:{s:?}"),
            Run::Market(a) => format!("market:{a}"),
            Run::Term(i) => format!("term:{i}"),
            Run::RemoveAlert(id) => format!("alert:{id}"),
            Run::WritePinned => "chat:write".into(),
            Run::Pin(t, _) => format!("chat:pin:{}", t.as_deref().unwrap_or("")),
            Run::Subscribe(t, _) => format!("chat:sub:{t}"),
            Run::Send { .. } | Run::Swap { .. } => format!("do:{}", self.label),
        }
    }
}

impl Entry {
    /// The optional feature this entry needs.
    fn feature(&self) -> Option<Feature> {
        match &self.run {
            Run::Action(id) => super::app::action_feature(id),
            Run::Go(screen) => screen.feature(),
            Run::Market(_) | Run::Swap { .. } => Some(Feature::Trading),
            Run::WritePinned | Run::Pin(..) | Run::Subscribe(..) => Some(Feature::Messaging),
            Run::Send { .. } | Run::Term(_) | Run::RemoveAlert(_) => None,
        }
    }
}

/// How many recent choices are kept and shown.
pub const RECENTS: usize = 6;

impl App {
    /// Everything the palette offers for `query`, best first, less what belongs to a feature
    /// that is turned off.
    pub fn palette_entries(&self, query: &str) -> Vec<Entry> {
        let mut out = self.all_palette_entries(query);
        out.retain(|e| e.feature().is_none_or(|f| self.config.features.on(f)));
        out
    }

    fn all_palette_entries(&self, query: &str) -> Vec<Entry> {
        let mut out: Vec<Entry> = self.palette_intents(query);
        let all = self.palette_candidates();
        if query.trim().is_empty() {
            // Recents first, then the actions in their usual order.
            for key in &self.palette_recent {
                if let Some(e) = all.iter().find(|e| e.key() == *key) {
                    out.push(Entry { tag: "recent", ..e.clone() });
                }
            }
            out.extend(all.into_iter().filter(|e| e.tag == "action" && !self.palette_recent.contains(&e.key())));
            return out;
        }
        use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
        use nucleo_matcher::{Config, Matcher};
        let mut matcher = Matcher::new(Config::DEFAULT);
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let mut scored: Vec<(u32, Entry)> = all
            .into_iter()
            .filter_map(|e| {
                let mut buf = Vec::new();
                let hay = format!("{} {}", e.label, e.hint);
                let score = pattern.score(nucleo_matcher::Utf32Str::new(&hay, &mut buf), &mut matcher)?;
                // What was used recently ranks a little higher among equals.
                let boost = if self.palette_recent.contains(&e.key()) { 20 } else { 0 };
                Some((score + boost, e))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        out.extend(scored.into_iter().map(|(_, e)| e));
        out
    }

    /// Actions, screens, contacts, holdings and markets.
    fn palette_candidates(&self) -> Vec<Entry> {
        let mut out: Vec<Entry> = ACTIONS.iter().map(action_entry).collect();
        for s in super::app::Section::ALL.iter().flat_map(|sec| sec.screens(&self.config.features)) {
            out.push(Entry {
                tag: "go",
                label: format!("Go to {} › {}", s.section().title(), s.title()),
                hint: super::keymap::chord(s),
                cli: String::new(),
                run: Run::Go(s),
            });
        }
        for c in &self.dash.contacts {
            let qi = c.payment_code.is_some()
                || c.address
                    .as_deref()
                    .and_then(|a| wallet_core::registry::parse_any_address(a).ok())
                    .is_some_and(|a| a.ledger() == wallet_core::sdk::Ledger::Qi);
            let asset = if qi { "Qi" } else { "QUAI" };
            out.push(Entry {
                tag: "contact",
                label: format!("Send {asset} to {}", c.name),
                hint: c.address.as_deref().map(wallet_core::session::short_address).unwrap_or_else(|| "payment code".into()),
                cli: format!("quai-terminal send {} --to \"{}\" --amount N", asset.to_lowercase(), c.name),
                run: Run::Send {
                    kind: if qi { FormKind::SendQi } else { FormKind::SendQuai },
                    to: c.name.clone(),
                    amount: String::new(),
                    token: None,
                },
            });
        }
        // Chats: write to the pinned one; pin or subscribe to any followed channel or peer.
        if let Some(pin) = &self.eco.board.pin {
            let label = self.chat_label(pin);
            out.push(Entry {
                tag: "chat",
                label: format!("Write in {label}"),
                hint: "`".into(),
                cli: String::new(),
                run: Run::WritePinned,
            });
            out.push(Entry {
                tag: "chat",
                label: format!("Unpin {label}"),
                hint: String::new(),
                cli: String::new(),
                run: Run::Pin(None, label),
            });
        }
        for row in self.board_rows().iter().filter(|r| !matches!(r, super::eco::BoardRow::Unfollowed(..))) {
            let (target, label) = App::chat_target(row);
            if self.eco.board.pin.as_deref() != Some(target.as_str()) {
                out.push(Entry {
                    tag: "chat",
                    label: format!("Pin {label} beside every screen"),
                    hint: String::new(),
                    cli: String::new(),
                    run: Run::Pin(Some(target.clone()), label.clone()),
                });
            }
            let on = self.eco.board.subs.contains(&target);
            out.push(Entry {
                tag: "chat",
                label: if on { format!("Stop notifications from {label}") } else { format!("Notify me about {label}") },
                hint: String::new(),
                cli: String::new(),
                run: Run::Subscribe(target, label),
            });
        }
        for a in &self.eco.alerts {
            out.push(Entry {
                tag: "alert",
                label: format!("Remove alert: {}", a.describe()),
                hint: if a.fired > 0 {
                    format!("fired {}", super::ui::ago(wallet_core::registry::now().saturating_sub(a.fired)))
                } else {
                    "not fired yet".into()
                },
                cli: format!("quai-terminal alert rm {}", a.id),
                run: Run::RemoveAlert(a.id),
            });
        }
        for (i, term) in super::glossary::TERMS.iter().enumerate() {
            out.push(Entry {
                tag: "term",
                label: format!("What is {}?", term.word),
                hint: term.meaning.to_string(),
                cli: String::new(),
                run: Run::Term(i),
            });
        }
        if let Some(p) = &self.eco.portfolio {
            for r in &p.rows {
                let asset = match &r.key {
                    wallet_core::portfolio::AssetKey::Quai => SwapAsset::Quai,
                    wallet_core::portfolio::AssetKey::Token(a) => {
                        SwapAsset::Token { address: a.to_lowercase(), symbol: r.symbol.clone(), decimals: r.decimals }
                    }
                    _ => continue,
                };
                out.push(Entry {
                    tag: "asset",
                    label: format!("Swap {}", r.symbol),
                    hint: r.name.clone(),
                    cli: format!("quai-terminal swap --from {} --to TOKEN --amount N", r.symbol),
                    run: Run::Swap { from: asset, to: None, amount: String::new() },
                });
            }
        }
        if let Some(Ok((pools, _))) = &self.eco.markets_view.pools {
            for p in pools {
                let base0 = self.pool_base0(p);
                let (b, q) = if base0 { (&p.token0, &p.token1) } else { (&p.token1, &p.token0) };
                out.push(Entry {
                    tag: "market",
                    label: format!("{}/{} chart", self.market_symbol(b), self.market_symbol(q)),
                    hint: p.tvl_usd.map(|v| format!("{} TVL", wallet_core::swap::usd_compact(v))).unwrap_or_default(),
                    cli: String::new(),
                    run: Run::Market(p.address.clone()),
                });
            }
        }
        out
    }

    /// Entries read straight from the query: `send alice 5 quai`, `send 5 qi to bob`,
    /// `swap 10 wqi to usdt`, `buy laptop with quai`.
    fn palette_intents(&self, query: &str) -> Vec<Entry> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let Some(verb) = words.first() else { return vec![] };
        let rest: Vec<&str> = words[1..].iter().map(String::as_str).filter(|w| !matches!(*w, "to" | "for" | "with" | "→" | "of")).collect();
        let number = |w: &str| w.parse::<f64>().is_ok_and(|v| v > 0.0);
        let amount = rest.iter().find(|w| number(w)).map(|w| w.to_string()).unwrap_or_default();
        let words_only: Vec<&str> = rest.iter().copied().filter(|w| !number(w)).collect();
        match verb.as_str() {
            "send" | "pay" | "give" => {
                // The asset is whichever word names one; the rest is who.
                let (asset, who): (Option<&str>, Vec<&str>) = match words_only.iter().position(|w| self.palette_send_asset(w).is_some()) {
                    Some(i) if words_only.len() > 1 => {
                        (Some(words_only[i]), words_only.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, w)| *w).collect())
                    }
                    _ => (None, words_only.clone()),
                };
                let who = who.join(" ");
                if who.is_empty() {
                    return vec![];
                }
                let to = self.palette_recipient(&who);
                let (kind, token, label_asset) = match asset.and_then(|a| self.palette_send_asset(a)) {
                    Some(SendAsset::Qi) => (FormKind::SendQi, None, "Qi".to_string()),
                    Some(SendAsset::Token(sym)) => (FormKind::SendToken, Some(sym.clone()), sym),
                    _ => (FormKind::SendQuai, None, "QUAI".to_string()),
                };
                let what = if amount.is_empty() { label_asset.clone() } else { format!("{amount} {label_asset}") };
                vec![Entry {
                    tag: "do",
                    label: format!("Send {what} to {to}"),
                    hint: "opens the form · review decides".into(),
                    cli: format!(
                        "quai-terminal send {} --to \"{to}\" --amount {}",
                        token.clone().unwrap_or_else(|| label_asset.to_lowercase()),
                        if amount.is_empty() { "N" } else { &amount }
                    ),
                    run: Run::Send { kind, to, amount, token },
                }]
            }
            "swap" | "trade" | "sell" | "buy" | "convert" if !words_only.is_empty() => {
                let assets: Vec<SwapAsset> = words_only.iter().filter_map(|w| self.palette_swap_asset(w)).collect();
                let (from, to) = match (verb.as_str(), assets.as_slice()) {
                    ("buy", [want]) => (SwapAsset::Quai, Some(want.clone())),
                    ("buy", [want, with, ..]) => (with.clone(), Some(want.clone())),
                    (_, [from]) => (from.clone(), None),
                    (_, [from, to, ..]) => (from.clone(), Some(to.clone())),
                    _ => return vec![],
                };
                // Buying names what arrives; the card is sized by what is paid, so no amount.
                let amount = if verb == "buy" { String::new() } else { amount };
                let to_text = to.as_ref().map(|t| format!(" → {}", t.symbol())).unwrap_or_default();
                let what = if amount.is_empty() { from.symbol().to_string() } else { format!("{amount} {}", from.symbol()) };
                vec![Entry {
                    tag: "do",
                    label: format!("Swap {what}{to_text}"),
                    hint: "opens the swap card · review decides".into(),
                    cli: String::new(),
                    run: Run::Swap { from, to, amount },
                }]
            }
            _ => vec![],
        }
    }

    /// A contact's name as saved (they resolve by name), an address as typed, or the words.
    fn palette_recipient(&self, who: &str) -> String {
        if who.starts_with("0x") {
            return who.to_string();
        }
        let exact = self.dash.contacts.iter().find(|c| c.name.eq_ignore_ascii_case(who));
        let prefix = || self.dash.contacts.iter().find(|c| c.name.to_lowercase().starts_with(who));
        exact.or_else(prefix).map(|c| c.name.clone()).unwrap_or_else(|| who.to_string())
    }

    fn palette_send_asset(&self, word: &str) -> Option<SendAsset> {
        match word {
            "quai" => Some(SendAsset::Quai),
            "qi" => Some(SendAsset::Qi),
            _ => self.eco.portfolio.as_ref()?.rows.iter().find_map(|r| {
                (matches!(r.key, wallet_core::portfolio::AssetKey::Token(_)) && r.symbol.eq_ignore_ascii_case(word))
                    .then(|| SendAsset::Token(r.symbol.clone()))
            }),
        }
    }

    /// A swap asset by symbol: QUAI, anything held, or anything the market lists.
    fn palette_swap_asset(&self, word: &str) -> Option<SwapAsset> {
        if word == "quai" {
            return Some(SwapAsset::Quai);
        }
        let held = self.eco.portfolio.as_ref().and_then(|p| {
            p.rows.iter().find_map(|r| match &r.key {
                wallet_core::portfolio::AssetKey::Token(a) if r.symbol.eq_ignore_ascii_case(word) => {
                    Some(SwapAsset::Token { address: a.to_lowercase(), symbol: r.symbol.clone(), decimals: r.decimals })
                }
                _ => None,
            })
        });
        held.or_else(|| {
            let Some(Ok((pools, _))) = &self.eco.markets_view.pools else { return None };
            pools.iter().flat_map(|p| [&p.token0, &p.token1]).find_map(|t| {
                (t.symbol.eq_ignore_ascii_case(word) || self.market_symbol(t).eq_ignore_ascii_case(word)).then(|| SwapAsset::Token {
                    address: t.address.to_lowercase(),
                    symbol: self.market_symbol(t),
                    decimals: t.decimals,
                })
            })
        })
    }

    /// Do what a palette entry says, and remember it.
    pub fn run_palette(&mut self, entry: Entry) {
        self.modal = Modal::None;
        let key = entry.key();
        self.palette_recent.retain(|k| *k != key);
        self.palette_recent.insert(0, key);
        self.palette_recent.truncate(RECENTS);
        self.save_palette_recent();
        match entry.run {
            Run::Action(id) => self.run_action(id),
            Run::Term(i) => self.modal = Modal::Glossary { selected: i },
            Run::RemoveAlert(id) => self.send_data(super::data::DataCmd::Alerts(super::data::AlertOp::Remove(id))),
            Run::WritePinned => self.write_pinned(),
            Run::Pin(target, label) => self.send(super::worker::Cmd::Chat(super::worker::ChatOp::Pin { target, label })),
            Run::Subscribe(target, label) => self.send(super::worker::Cmd::Chat(super::worker::ChatOp::Toggle { target, label })),
            Run::Go(screen) => self.switch(screen),
            Run::Market(address) => {
                self.switch(Screen::Markets);
                if let Some(Ok((pools, _))) = &self.eco.markets_view.pools
                    && let Some(i) = pools.iter().position(|p| p.address == address)
                {
                    self.pane = 0;
                    self.selected = i;
                    self.eco.markets_view.pair_selected = i;
                }
            }
            Run::Swap { from, to, amount } => {
                self.switch(Screen::Swap);
                let card = &mut self.eco.swap;
                card.from = from;
                if to.is_some() {
                    card.to = to;
                }
                card.amount = amount;
                card.quote = None;
                card.preset = None;
                card.field = 1;
                card.edited = Some(std::time::Instant::now());
            }
            Run::Send { kind, to, amount, token } => {
                if !self.can_sign() {
                    self.toast("this wallet is watch-only", true);
                    return;
                }
                self.open_form(kind.clone());
                if let Modal::Form(f) = &mut self.modal {
                    let mut set = |label: &str, value: &str| {
                        if let Some(field) = f.fields.iter_mut().find(|fl| fl.label == label)
                            && !value.is_empty()
                        {
                            field.value = value.to_string();
                        }
                    };
                    set("To", &to);
                    set("Amount", &amount);
                    set("Token", token.as_deref().unwrap_or_default());
                    // The cursor lands on the first thing still to fill, or on Amount to confirm.
                    f.focus = f
                        .fields
                        .iter()
                        .position(|fl| fl.value.is_empty() && !fl.optional)
                        .or_else(|| f.fields.iter().position(|fl| fl.label == "Amount"))
                        .unwrap_or(0);
                }
            }
        }
    }

    fn palette_recent_path(&self) -> Option<std::path::PathBuf> {
        self.meta.as_ref().map(|m| self.paths.wallet_dir(&m.id).join("palette_recent"))
    }

    /// Recents for the open wallet (one key per line, in the wallet's own directory).
    pub fn load_palette_recent(&mut self) {
        self.palette_recent = self
            .palette_recent_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| s.lines().filter(|l| !l.is_empty()).take(RECENTS).map(str::to_string).collect())
            .unwrap_or_default();
    }

    fn save_palette_recent(&self) {
        if let Some(p) = self.palette_recent_path() {
            let _ = std::fs::write(p, self.palette_recent.join("\n"));
        }
    }
}

enum SendAsset {
    Quai,
    Qi,
    Token(String),
}

fn action_entry(a: &'static Action) -> Entry {
    Entry { tag: "action", label: a.label.to_string(), hint: keys_for(a.id), cli: a.cli.to_string(), run: Run::Action(a.id) }
}

/// The keys that reach an action, worked out from the keymap: a verb's own key when the action
/// is that verb anywhere, else the chord to the screen whose table runs it and the key there
/// ("g q space s"), else the chord to the screen it is. Empty when only the palette reaches it.
pub fn keys_for(id: &str) -> String {
    use super::app::verbs::{Do, view_keys};
    use super::keymap::{self, Verb};
    let verb = match id {
        "send_quai" => Some(Verb::Send),
        "receive_quai" => Some(Verb::Receive),
        "trade" | "swap" => Some(Verb::Trade),
        "convert_quai_qi" => Some(Verb::Convert),
        "lock" => Some(Verb::Lock),
        "notifications" => Some(Verb::Notifications),
        "help" => Some(Verb::Help),
        "quit" => Some(Verb::Quit),
        "refresh" => Some(Verb::RefreshAll),
        _ => None,
    };
    if let Some(v) = verb {
        return keymap::key_of(v);
    }
    for screen in super::app::Screen::ALL_SCREENS {
        let keys = view_keys(screen, None);
        let chord = keymap::chord(screen);
        if let Some(o) = keys.overrides.iter().find(|o| matches!(o.how, Do::Run(r) if r == id)) {
            return format!("{chord} {}", keymap::key_of(o.verb));
        }
        if let Some(item) = keys.sheet.iter().find(|i| matches!(i.how, Do::Run(r) if r == id)) {
            return format!("{chord} space {}", item.key);
        }
    }
    let screen = match id {
        "portfolio" | "home" => Some(super::app::Screen::Home),
        "nfts" => Some(super::app::Screen::Collected),
        "explore" => Some(super::app::Screen::Explore),
        "listings" => Some(super::app::Screen::Listings),
        "locks" => Some(super::app::Screen::Accounts),
        "launches" => Some(super::app::Screen::Launches),
        "pnl" => Some(super::app::Screen::Pnl),
        "contacts" => Some(super::app::Screen::Contacts),
        "data_sources" => Some(super::app::Screen::DataSources),
        "network" => Some(super::app::Screen::Network),
        "wrap_qi" | "claim_wqi" | "unwrap_wqi" | "wrap_quai" | "unwrap_quai" => Some(super::app::Screen::Wrap),
        "quote" | "convert_qi_quai" => Some(super::app::Screen::Convert),
        _ => None,
    };
    screen.map(keymap::chord).unwrap_or_default()
}
