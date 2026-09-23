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
}

pub async fn handle(session: &mut Session, request: Request) -> Result<Ev> {
    match request {
        Request::List => {}
        Request::Create(request) => {
            orders::create(session, request).await?;
        }
        Request::Observe(id) => {
            session.track().await?;
            orders::observe(session, &id).await?;
        }
        Request::Cancel(id) => {
            orders::cancel(session, &id)?;
        }
    }
    Ok(Ev::Orders { wallet: session.meta.id.clone(), network: session.network.id.clone(), rows: orders::list(session)? })
}

pub fn fields(account: Field) -> Vec<Field> {
    vec![
        account,
        Field::new("Minimum receive", "fixed input limit; receive token units"),
        Field::new("Maximum fee per attempt", "QUAI").amount("QUAI"),
        Field::new("Total fee authorization", "QUAI; includes approvals and declined attempts").amount("QUAI"),
        Field::new("Expires in minutes", "2 through 43200").with("60"),
        Field::new("Maximum attempts", "1 through 8; each approval is an attempt").with("3"),
    ]
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
    let minutes = value(4).parse::<u64>().map_err(|_| CoreError::Invalid("expiry must be a whole number of minutes".into()))?;
    let attempts = value(5).parse::<u8>().map_err(|_| CoreError::Invalid("attempts must be a whole number from 1 through 8".into()))?;
    if !(2..=43200).contains(&minutes) || !(1..=8).contains(&attempts) {
        return Err(CoreError::Invalid("expiry must be 2..43200 minutes and attempts 1..8".into()));
    }
    Ok(Request::Create(orders::Create {
        account,
        from,
        to,
        input,
        minimum_output: value(1),
        slippage_bps: slippage,
        expires_at: wallet_core::registry::now() + minutes * 60,
        maximum_fee: value(2),
        total_fee_budget: value(3),
        max_attempts: attempts,
        mode: orders::Mode::Trigger,
    }))
}

/// The Orders screen's actions, on the order under the cursor.
impl App {
    fn order_selected(&self) -> Option<TradePlan> {
        self.eco.orders.as_ref().and_then(|rows| rows.get(self.selected)).cloned()
    }

    /// Read the order book again.
    pub(crate) fn orders_reload(&mut self) {
        self.send(Cmd::Order(Request::List));
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
        .filter_map(|p| orders::details(p).ok().filter(|v| v.state == orders::State::Triggered).map(|v| (p.id.clone(), v.spec.to.clone())))
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
            "No limit orders. Each one waits for its price, then asks you for a fresh review.",
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
            let state = view.as_ref().map(|v| format!("{:?}", v.state)).unwrap_or_else(|_| "invalid record".into());
            let style = if i == selected { t.selected() } else { t.text_style() };
            // A reachable limit keeps an attention mark until it is reviewed or stops.
            let ready = view.as_ref().is_ok_and(|v| v.state == orders::State::Triggered);
            let mark = ratatui::text::Span::styled(if ready { "▌" } else { " " }, ratatui::style::Style::default().fg(t.attention));
            Line::from(vec![mark, ratatui::text::Span::styled(format!("{:<18} {state}", super::ui::truncate(&p.id, 18)), style)])
        })
        .collect();
    let terms = rows.get(selected).map(|plan| order_terms(t, plan)).unwrap_or_default();
    match column {
        Some(column) => {
            let block = super::ui::panel(t, &format!("order · {}", super::ui::truncate(&rows[selected].id, 30)), false);
            let frame_h = column.height.saturating_sub(block.inner(column).height);
            let inner_w = block.inner(column).width.max(1) as usize;
            let wrapped: u16 = terms.iter().map(|l| l.width().max(1).div_ceil(inner_w) as u16).sum();
            let rect = Rect { height: (wrapped + frame_h).min(column.height), ..column };
            f.render_widget(Paragraph::new(terms).wrap(Wrap { trim: false }).block(block), rect);
        }
        None => {
            lines.push(Line::from(""));
            lines.extend(terms);
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// What an order will do: what it pays and at least what it gets back, who signs and through
/// which router, how often it may try, and what that may cost.
fn order_terms(t: &Theme, plan: &TradePlan) -> Vec<Line<'static>> {
    match orders::details(plan) {
        Ok(v) => {
            let quantity = |raw: &str, d: u8| {
                wallet_core::sdk::U256::from_str_radix(raw, 10).map(|v| super::num::short(v, d, 6)).unwrap_or_else(|_| "invalid".into())
            };
            let kv = |k: &str, v: String| super::widgets::kv(t, k, vec![ratatui::text::Span::styled(v, t.text_style())]);
            vec![
                kv("pay", format!("{} {}", quantity(&v.spec.input_atoms, v.spec.input_decimals), v.spec.from)),
                kv("receive", format!("at least {} {}", quantity(&v.spec.minimum_output_atoms, v.spec.output_decimals), v.spec.to)),
                kv("signer", v.spec.account.clone()),
                kv("router", format!("{} (pinned)", v.spec.router)),
                kv("attempts", format!("{} of {} · {:?}", v.attempts.len(), v.spec.max_attempts, v.spec.mode)),
                kv(
                    "fees",
                    format!(
                        "up to {} QUAI an attempt · {} of {} QUAI used",
                        quantity(&v.spec.maximum_fee_atoms, 18),
                        quantity(&v.fee_budget_used_atoms, 18),
                        quantity(&v.spec.total_fee_budget_atoms, 18)
                    ),
                ),
                Line::from(""),
                Line::styled(plan.reason.clone(), t.dim_style()),
                Line::styled("Cancelling stops future signing; anything already signed stays tracked.", t.dim_style()),
            ]
        }
        Err(e) => vec![Line::styled(e.to_string(), ratatui::style::Style::default().fg(t.danger))],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tui_creation_is_trigger_only_and_rejects_malformed_bounds() {
        let mut fields = fields(Field::new("Account", ""));
        fields[1].value = "10".into();
        fields[2].value = "0.01".into();
        fields[3].value = "0.03".into();
        let request = create_request(None, "quai".into(), "token".into(), "1".into(), 50, &fields).unwrap();
        assert!(matches!(request, Request::Create(orders::Create { mode: orders::Mode::Trigger, max_attempts: 3, .. })));
        fields[5].value = "3.5".into();
        assert!(create_request(None, "quai".into(), "token".into(), "1".into(), 50, &fields).is_err());
        fields[5].value = "3".into();
        fields[4].value = "0".into();
        assert!(create_request(None, "quai".into(), "token".into(), "1".into(), 50, &fields).is_err());
    }
}
