//! Interactive limit-order controls. Observation is read-only; every execution opens a review.
use super::app::{App, Field};
use super::theme::Theme;
use super::worker::{Cmd, Ev, Prepare};
use ratatui::{
    Frame,
    layout::Rect,
    text::Line,
    widgets::{Paragraph, Wrap},
};
use wallet_core::{CoreError, Result, orders, plans::TradePlan, session::Session};

#[derive(Clone, Debug)]
pub enum Request {
    List,
    Create(orders::Create),
    Observe(String),
    Cancel(String),
    /// The background check: every active order re-quoted, quietly. A quote that fails leaves
    /// that order as it was; the next check tries again.
    Watch,
}

/// How often the open terminal re-checks active orders. One quote per order each time, against
/// the same node and explorer budget as everything else.
pub const WATCH_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

pub async fn handle(session: &mut Session, request: Request) -> Result<Ev> {
    let mut announced = Vec::new();
    match request {
        Request::List => {}
        Request::Watch => {
            let _ = session.track().await;
            for plan in orders::list(session)? {
                if orders::details(&plan).is_ok_and(|v| v.state.active())
                    && orders::observe(session, &plan.id).await.is_ok_and(|seen| seen.announced)
                {
                    announced.push(plan.id);
                }
            }
        }
        Request::Create(request) => {
            orders::create(session, request).await?;
        }
        Request::Observe(id) => {
            session.track().await?;
            if orders::observe(session, &id).await?.announced {
                announced.push(id);
            }
        }
        Request::Cancel(id) => {
            orders::cancel(session, &id)?;
        }
    }
    Ok(Ev::Orders { wallet: session.meta.id.clone(), network: session.network.id.clone(), rows: orders::list(session)?, announced })
}

/// What the order form knows about the swap it was opened from: the Swap card's live quote.
/// It drives the preview; the order itself is made against a fresh quote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    pub from_symbol: String,
    pub to_symbol: String,
    pub from_decimals: u8,
    pub to_decimals: u8,
    pub input_atoms: String,
    /// What the swap returns now, before slippage.
    pub current_out: String,
}

// Field order, read back by `create_request`.
const TARGET: usize = 1;
const EXPIRES: usize = 2;
const ATTEMPTS: usize = 3;
const FEE: usize = 4;
const BUDGET: usize = 5;

/// The order form: the account, what to wait for and until when, then the fee bounds, filled in
/// with defaults that never refuse a normal swap (`orders::default_fees`).
pub fn fields(account: Field, preview: &Preview, fees: (String, String)) -> Vec<Field> {
    let expiries = [(3_600, "1 hour"), (86_400, "1 day"), (604_800, "1 week"), (2_592_000, "30 days")];
    vec![
        account,
        Field::new("Target", &format!("+5% = 5% more {} back than now · or an amount to receive", preview.to_symbol)).with("+5%"),
        Field::new("Expires", "←/→ to choose").with("86400").choice(expiries.iter().map(|(s, l)| (s.to_string(), l.to_string())).collect()),
        Field::new("Attempts", "advanced · 1 through 8; a token approval counts as one").with("3"),
        Field::new("Fee cap per attempt", "advanced · QUAI; the network's fee policy by default").with(fees.0).amount("QUAI"),
        Field::new("Total fee budget", "advanced · QUAI; declined attempts use it too").with(fees.1).amount("QUAI"),
    ]
}

/// What the order form says above its fields: what an order here is, then what this one would
/// wait for, recomputed from the fields as they are typed.
pub fn note(preview: &Preview, slippage_bps: u16, fields: &[Field]) -> String {
    use wallet_core::sdk::U256;
    let intro = "Quai has no order book, so nothing is posted on-chain. While Quai Terminal is open it re-checks this \
                 swap every 30 s; when the price reaches your target it tells you and asks you to review and sign. It \
                 never signs by itself.";
    let atoms = |raw: &str| U256::from_str_radix(raw, 10).unwrap_or_default();
    let (input, now) = (atoms(&preview.input_atoms), atoms(&preview.current_out));
    let show = |v: U256, d: u8| wallet_core::amount::group_thousands(&wallet_core::amount::format_amount_short(v, d, 6));
    // Both ways, since which one reads as "the price" depends on which side is the token.
    let rates = |out: U256| {
        if input.is_zero() || out.is_zero() {
            return String::new();
        }
        let scale = |d: u8| U256::from(10u64).pow(U256::from(d));
        let per_in = wallet_core::amount::mul_div(out, scale(preview.from_decimals), input).unwrap_or_default();
        let per_out = wallet_core::amount::mul_div(input, scale(preview.to_decimals), out).unwrap_or_default();
        format!(
            " (1 {} = {} {} · 1 {} = {} {})",
            preview.from_symbol,
            show(per_in, preview.to_decimals),
            preview.to_symbol,
            preview.to_symbol,
            show(per_out, preview.from_decimals),
            preview.from_symbol
        )
    };
    let now_line = format!(
        "Now {} {} → {} {}{}.",
        show(input, preview.from_decimals),
        preview.from_symbol,
        show(now, preview.to_decimals),
        preview.to_symbol,
        rates(now)
    );
    let typed = fields.get(TARGET).map(|f| f.value.as_str()).unwrap_or_default();
    let target_line = match orders::target_from_text(typed, now, preview.to_decimals) {
        Err(e) => format!("Target: {e}."),
        Ok(target) => {
            let minimum = orders::minimum_for_target(target, slippage_bps);
            let gap = match orders::distance_bps(now, target) {
                Some(d) if d > 0 => format!(" · needs +{}%", wallet_core::amount::format_amount(U256::from(d as u64), 2)),
                Some(_) => " · met now, so a plain swap would do the same".into(),
                None => String::new(),
            };
            format!(
                "Waits for {} {}{}{gap}. Guarantees at least {} {} after {}% slippage.",
                show(target, preview.to_decimals),
                preview.to_symbol,
                rates(target),
                show(minimum, preview.to_decimals),
                preview.to_symbol,
                wallet_core::amount::format_amount(U256::from(slippage_bps), 2)
            )
        }
    };
    format!("{intro}\n\n{now_line}\n{target_line}")
}

pub fn create_request(
    account: Option<String>,
    from: String,
    to: String,
    input: String,
    slippage: u16,
    fields: &[Field],
) -> Result<Request> {
    let value = |i: usize| fields.get(i).map(|f| f.value.trim().to_string()).unwrap_or_default();
    let seconds = value(EXPIRES).parse::<u64>().map_err(|_| CoreError::Invalid("choose when the order expires".into()))?;
    let attempts =
        value(ATTEMPTS).parse::<u8>().map_err(|_| CoreError::Invalid("attempts must be a whole number from 1 through 8".into()))?;
    if !(120..=30 * 86_400).contains(&seconds) || !(1..=8).contains(&attempts) {
        return Err(CoreError::Invalid("expiry must be 2 minutes to 30 days and attempts 1 through 8".into()));
    }
    let target = value(TARGET);
    if target.is_empty() {
        return Err(CoreError::Invalid("type a target: +5% (better than now) or an amount to receive".into()));
    }
    Ok(Request::Create(orders::Create {
        account,
        from,
        to,
        input,
        minimum_output: String::new(),
        target_output: Some(target),
        slippage_bps: slippage,
        expires_at: wallet_core::registry::now() + seconds,
        maximum_fee: value(FEE),
        total_fee_budget: value(BUDGET),
        max_attempts: attempts,
        mode: orders::Mode::Trigger,
    }))
}

/// The Orders screen's actions, on the order under the cursor.
impl App {
    /// Re-check active limit orders every [`WATCH_EVERY`] while the terminal is open, locked or
    /// not: a check reads a quote and never signs. The first check also loads the list, so an
    /// order made in an earlier session is watched without visiting Orders. When one becomes
    /// reachable, `Ev::Orders` says so (toast, desktop notice, bell).
    pub(crate) fn tick_orders(&mut self) {
        if !self.config.features.on(wallet_core::config::Feature::Trading) || self.worker.is_none() || self.meta.is_none() {
            return;
        }
        if self.eco.orders_watched_at.is_some_and(|at| at.elapsed() < WATCH_EVERY) {
            return;
        }
        let first = self.eco.orders_watched_at.is_none();
        let active = self.eco.orders.as_deref().is_some_and(|rows| rows.iter().any(|p| orders::details(p).is_ok_and(|v| v.state.active())));
        self.eco.orders_watched_at = Some(std::time::Instant::now());
        if first || active {
            self.send(Cmd::Order(Request::Watch));
        }
    }

    fn order_selected(&self) -> Option<TradePlan> {
        self.eco.orders.as_ref().and_then(|rows| rows.get(self.selected)).cloned()
    }

    /// Read the orders again, as they were last checked.
    pub(crate) fn orders_list(&mut self) {
        self.send(Cmd::Order(Request::List));
    }

    /// Re-check every active order now, and read the list again.
    pub(crate) fn orders_reload(&mut self) {
        self.eco.orders_watched_at = Some(std::time::Instant::now());
        self.send(Cmd::Order(Request::Watch));
        self.info("checking your orders against fresh quotes…");
    }

    /// Re-read the chain for the order under the cursor.
    pub(crate) fn order_observe(&mut self) {
        if let Some(p) = self.order_selected() {
            self.send(Cmd::Order(Request::Observe(p.id)));
        }
    }

    /// Stop the order under the cursor: nothing further is signed for it.
    pub(crate) fn order_cancel(&mut self) {
        if let Some(p) = self.order_selected() {
            self.send(Cmd::Order(Request::Cancel(p.id)));
        }
    }

    /// A fresh review for the order under the cursor, if its limit is met.
    pub(crate) fn order_review(&mut self) {
        let Some(p) = self.order_selected() else { return };
        if self.locked {
            self.toast("unlock the wallet before requesting an order review", true);
        } else if self.eco.flow.is_some() {
            self.toast("finish or cancel the active trading flow before reviewing an order", true);
        } else {
            self.send(Cmd::Prepare(Prepare::OrderRun { id: p.id }));
            self.info("checking the limit and preparing one fresh review…");
        }
    }
}

/// The orders whose limit is met and wait for a review: (id, what they buy).
pub fn reachable(rows: &[TradePlan]) -> Vec<(String, String)> {
    rows.iter()
        .filter_map(|p| {
            orders::details(p)
                .ok()
                .filter(|v| v.state == orders::State::Triggered)
                .map(|v| (p.id.clone(), v.spec.to_symbol.clone().unwrap_or_else(|| v.spec.to.clone())))
        })
        .collect()
}

/// Trade › Orders: every limit order, the one under the cursor in full, and what it may cost.
pub fn draw_screen(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    use super::app::Screen;
    let rows: &[TradePlan] = app.eco.orders.as_deref().unwrap_or(&[]);
    // On a wide terminal the selected order's terms go in a column beside the list.
    let (area, column) = super::ui::with_inspector(app, area);
    let (area, column) = match column {
        Some(column) if !rows.is_empty() => {
            // The list is only as tall as its orders.
            let h = (rows.len() as u16 + 2).max(5).min(area.height);
            (Rect { height: h, ..area }, Some(column))
        }
        _ => (area, None),
    };
    let block = super::ui::panel(t, &format!("limit orders · {}", rows.len()), true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if app.eco.orders.is_none() {
        return super::ui::empty_state(f, inner, t, super::ui::spinner(), "Reading your orders…", &[]);
    }
    if rows.is_empty() {
        return super::ui::empty(
            f,
            inner,
            t,
            super::icons::Icon::Swap,
            "No limit orders. One waits for a price you choose, then asks you to review and sign; \
             Quai has no order book, so nothing is posted on-chain.",
            &[("g x", "swap · space o to create one")],
        );
    }
    let selected = app.selected.min(rows.len() - 1);
    let list_h = if column.is_some() { inner.height } else { (rows.len() as u16).min(inner.height.saturating_sub(10).max(3)) };
    let list = Rect { height: list_h, ..inner };
    let id = super::hit::ListId::Screen(Screen::Orders, 0);
    let start = app.list_window(id, selected, rows.len(), list_h as usize);
    app.hits.borrow_mut().rows(id, list, start, rows.len(), |i| rows.get(i).map(|p| p.id.clone()));
    let mut lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(start)
        .take(list_h as usize)
        .map(|(i, p)| {
            let view = orders::details(p);
            let style = if i == selected { t.selected() } else { t.text_style() };
            // A reachable limit keeps an attention mark until it is reviewed or stops.
            let ready = view.as_ref().is_ok_and(|v| v.state == orders::State::Triggered);
            let mark = ratatui::text::Span::styled(if ready { "▌" } else { " " }, ratatui::style::Style::default().fg(t.attention));
            let text = match &view {
                Ok(v) => row_text(v),
                Err(_) => format!("{} · unreadable record", super::ui::truncate(&p.id, 18)),
            };
            Line::from(vec![mark, ratatui::text::Span::styled(text, style)])
        })
        .collect();
    let terms = rows.get(selected).map(|plan| order_terms(t, plan)).unwrap_or_default();
    match column {
        Some(column) => {
            // The whole column: a height estimated from line widths came out short once labels and
            // word wrapping were counted, and cut the last line off.
            let block = super::ui::panel(t, &format!("order · {}", super::ui::truncate(&rows[selected].id, 30)), false);
            f.render_widget(Paragraph::new(terms).wrap(Wrap { trim: false }).block(block), column);
        }
        None => {
            lines.push(Line::from(""));
            lines.extend(terms);
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// One order as a list row: what it trades, where it stands, and how far its price is.
pub fn row_text(v: &orders::Record) -> String {
    let rows = orders::describe(v, wallet_core::registry::now());
    let get = |k: &str| rows.iter().find(|(l, _)| *l == k).map(|(_, t)| t.clone()).unwrap_or_default();
    let gap = if v.state == orders::State::Armed {
        get("waits for")
            .split(" · ")
            .skip(1)
            .find(|s| s.starts_with("needs"))
            .map(|s| format!(" · {}", s.trim_end_matches(')')))
            .unwrap_or_default()
    } else {
        String::new()
    };
    format!("{} · {}{gap}", get("trade"), v.state.label())
}

/// What an order will do, in the words `orders::describe` uses everywhere (the CLI's `order
/// show` too), then what the keys do here.
fn order_terms(t: &Theme, plan: &TradePlan) -> Vec<Line<'static>> {
    match orders::details(plan) {
        Ok(v) => {
            let kv = |k: &str, v: String| super::widgets::kv(t, k, vec![ratatui::text::Span::styled(v, t.text_style())]);
            let mut lines: Vec<Line<'static>> =
                orders::describe(&v, wallet_core::registry::now()).into_iter().map(|(k, text)| kv(k, text)).collect();
            lines.push(Line::from(""));
            let next = match v.state {
                orders::State::Triggered => "The price is there: enter prepares a fresh review to sign.",
                orders::State::Armed => "Checked every 30 s while Quai Terminal is open; you are told when it is reachable.",
                _ => "",
            };
            if !next.is_empty() {
                lines.push(Line::styled(next.to_string(), t.dim_style()));
            }
            if v.state.active() {
                lines.push(Line::styled("x cancels: nothing further is signed; anything already signed stays tracked.", t.dim_style()));
            }
            lines
        }
        Err(e) => vec![Line::styled(e.to_string(), ratatui::style::Style::default().fg(t.danger))],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10 QUAI for 1,000 USDT (6 decimals) now, 0.5% slippage.
    fn preview() -> Preview {
        Preview {
            from_symbol: "QUAI".into(),
            to_symbol: "USDT".into(),
            from_decimals: 18,
            to_decimals: 6,
            input_atoms: "10000000000000000000".into(),
            current_out: "1000000000".into(),
        }
    }

    #[test]
    fn tui_creation_is_trigger_only_and_rejects_malformed_bounds() {
        let mut fields = fields(Field::new("Account", ""), &preview(), ("25".into(), "75".into()));
        assert_eq!(fields[TARGET].value, "+5%", "a target is filled in: 5% better than now");
        assert_eq!(fields[EXPIRES].value, "86400", "a day by default");
        let request = create_request(None, "quai".into(), "token".into(), "10".into(), 50, &fields).unwrap();
        let Request::Create(create) = request else { panic!() };
        assert!(matches!(create.mode, orders::Mode::Trigger) && create.max_attempts == 3);
        assert_eq!(create.target_output.as_deref(), Some("+5%"));
        assert_eq!((create.maximum_fee.as_str(), create.total_fee_budget.as_str()), ("25", "75"));
        let expires_in = create.expires_at - wallet_core::registry::now();
        assert!((86_390..=86_400).contains(&expires_in), "{expires_in}");
        fields[ATTEMPTS].value = "3.5".into();
        assert!(create_request(None, "quai".into(), "token".into(), "10".into(), 50, &fields).is_err());
        fields[ATTEMPTS].value = "9".into();
        assert!(create_request(None, "quai".into(), "token".into(), "10".into(), 50, &fields).is_err());
        fields[ATTEMPTS].value = "3".into();
        fields[TARGET].value = " ".into();
        assert!(create_request(None, "quai".into(), "token".into(), "10".into(), 50, &fields).is_err(), "a target is required");
    }

    /// The form says what an order is here, what it would wait for and guarantee, and how far
    /// that is from now, from whatever is typed.
    #[test]
    fn the_note_explains_the_order_and_follows_the_target() {
        let p = preview();
        let mut fields = fields(Field::new("Account", ""), &p, ("25".into(), "75".into()));
        let note = note(&p, 50, &fields);
        assert!(note.contains("no order book") && note.contains("never signs by itself"), "{note}");
        assert!(note.contains("Now 10 QUAI → 1,000 USDT (1 QUAI = 100 USDT · 1 USDT = 0.01 QUAI)"), "{note}");
        assert!(note.contains("Waits for 1,050 USDT"), "{note}");
        assert!(note.contains("needs +5%"), "{note}");
        assert!(note.contains("Guarantees at least 1,044.75 USDT after 0.5% slippage"), "{note}");
        // A plain amount is an amount to receive.
        fields[TARGET].value = "1,100".into();
        assert!(note_for(&p, &fields).contains("Waits for 1,100 USDT") && note_for(&p, &fields).contains("needs +10%"));
        // A target already met says a plain swap would do.
        fields[TARGET].value = "-1%".into();
        assert!(note_for(&p, &fields).contains("met now"), "{}", note_for(&p, &fields));
        fields[TARGET].value = "soon".into();
        assert!(note_for(&p, &fields).contains("Target: a target is"), "{}", note_for(&p, &fields));
    }

    fn note_for(p: &Preview, fields: &[Field]) -> String {
        note(p, 50, fields)
    }
}
