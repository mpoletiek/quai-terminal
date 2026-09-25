//! Limit orders as the engine runs them: list, create, observe, cancel, and the quiet background
//! check. Observation is read-only; every execution opens a review.

use crate::worker::Ev;
use wallet_core::{Result, orders, session::Session};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
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
