//! Interactive limit-order controls. Observation is read-only; every execution opens a review.
use super::app::{App, Field, Modal};
use super::theme::Theme;
use super::worker::{Cmd, Ev, Prepare};
use crossterm::event::{KeyCode, KeyEvent};
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

pub fn key(app: &mut App, rows: Vec<TradePlan>, mut selected: usize, key: KeyEvent) -> Modal {
    selected = selected.min(rows.len().saturating_sub(1));
    match key.code {
        KeyCode::Esc => return Modal::None,
        KeyCode::Char('j') | KeyCode::Down => selected = (selected + 1).min(rows.len().saturating_sub(1)),
        KeyCode::Char('k') | KeyCode::Up => selected = selected.saturating_sub(1),
        KeyCode::Char('R') => app.send(Cmd::Order(Request::List)),
        KeyCode::Char('o') => {
            if let Some(p) = rows.get(selected) {
                app.send(Cmd::Order(Request::Observe(p.id.clone())));
            }
        }
        KeyCode::Char('x') => {
            if let Some(p) = rows.get(selected) {
                app.send(Cmd::Order(Request::Cancel(p.id.clone())));
            }
        }
        KeyCode::Enter => {
            if let Some(p) = rows.get(selected) {
                if app.locked {
                    app.toast("unlock the wallet before requesting an order review", true);
                } else if app.eco.flow.is_some() {
                    app.toast("finish or cancel the active trading flow before reviewing an order", true);
                } else {
                    app.send(Cmd::Prepare(Prepare::OrderRun { id: p.id.clone() }));
                    app.toast("checking the limit and preparing one fresh review…", false);
                }
            }
        }
        _ => {}
    }
    Modal::Orders { rows, selected }
}

pub fn draw(f: &mut Frame, area: Rect, t: &Theme, rows: &[TradePlan], selected: usize) {
    let rect = super::ui::centered(area, 100, 28);
    let inner = super::ui::modal_frame(f, rect, t, "Limit orders · interactive review");
    let mut lines = vec![
        Line::from("j/k select · o observe · enter fresh review · x cancel · R reload · esc close"),
        Line::from("Cancellation stops future signing; signed transactions remain tracked."),
        Line::from(""),
    ];
    if rows.is_empty() {
        lines.push(Line::from("No orders. Close this panel and press O on Swap to create one."));
    }
    let start = selected.saturating_sub(2);
    for (i, p) in rows.iter().enumerate().skip(start).take(5) {
        let state = orders::details(p).map(|v| format!("{:?}", v.state)).unwrap_or_else(|_| "invalid record".into());
        lines.push(Line::from(format!("{} {}  {}", if i == selected { ">" } else { " " }, p.id, state)));
    }
    if let Some(plan) = rows.get(selected) {
        lines.push(Line::from(""));
        match orders::details(plan) {
            Ok(v) => {
                let quantity = |raw: &str, d: u8| {
                    wallet_core::sdk::U256::from_str_radix(raw, 10)
                        .map(|v| wallet_core::amount::format_amount(v, d))
                        .unwrap_or_else(|_| "invalid".into())
                };
                lines.extend([
                    Line::from(format!("Pay {} {}", quantity(&v.spec.input_atoms, v.spec.input_decimals), v.spec.from)),
                    Line::from(format!(
                        "Receive at least {} {}",
                        quantity(&v.spec.minimum_output_atoms, v.spec.output_decimals),
                        v.spec.to
                    )),
                    Line::from(format!("Signer {}", v.spec.account)),
                    Line::from(format!("Pinned router {}", v.spec.router)),
                    Line::from(format!(
                        "Expires {} · attempts {}/{} · {:?}",
                        v.spec.expires_at,
                        v.attempts.len(),
                        v.spec.max_attempts,
                        v.spec.mode
                    )),
                    Line::from(format!(
                        "Fee per attempt {} QUAI; authorization consumed {} / {} QUAI",
                        quantity(&v.spec.maximum_fee_atoms, 18),
                        quantity(&v.fee_budget_used_atoms, 18),
                        quantity(&v.spec.total_fee_budget_atoms, 18)
                    )),
                    Line::from(plan.reason.clone()),
                ]);
            }
            Err(e) => lines.push(Line::from(e.to_string())),
        }
    }
    f.render_widget(Paragraph::new(lines).style(t.text_style()).wrap(Wrap { trim: false }), inner);
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
