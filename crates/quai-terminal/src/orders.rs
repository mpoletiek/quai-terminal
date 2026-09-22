//! Client-side order controls. Observation never unlocks; bounded execution is explicitly opted in.
use crate::commands::Ctx;
use clap::{Args, Subcommand};
use wallet_core::orders::{self, Mode};
use wallet_core::{CoreError, Result};

#[derive(Args, Debug)]
pub struct OrderArgs {
    #[command(subcommand)]
    pub command: OrderCmd,
}

#[derive(Subcommand, Debug)]
pub enum OrderCmd {
    /// Arm a fixed-input limit trigger, bound to the currently selected pinned router.
    Create {
        from: String,
        to: String,
        amount: String,
        #[arg(long)]
        min_receive: String,
        #[arg(long)]
        account: Option<String>,
        /// Maximum fee per approval or swap, in QUAI.
        #[arg(long)]
        max_fee: String,
        /// Total fee authorization; failed or declined attempts consume this budget too.
        #[arg(long)]
        total_fee_budget: String,
        #[arg(long, default_value_t = 3)]
        max_attempts: u8,
        #[arg(long, default_value_t = 50)]
        slippage: u16,
        #[arg(long, default_value_t = 3600)]
        expires_in: u32,
        /// Permit explicit bounded execution in an unlocked client. Never authorizes the daemon.
        #[arg(long)]
        execute_once: bool,
    },
    /// List saved orders for the selected wallet and network.
    List,
    Show {
        id: String,
    },
    /// Observe fresh on-chain quotes without unlocking or signing. Stops when triggered.
    Observe {
        id: String,
        #[arg(long, default_value_t = 1)]
        samples: u16,
        #[arg(long, default_value_t = 5)]
        interval: u64,
    },
    /// Advance at most one approval or atomic swap after a fresh quote and review.
    Run {
        id: String,
        /// Requires an execute-once order, --yes and an explicit --authorization-policy.
        #[arg(long)]
        bounded: bool,
    },
    /// Cancel future execution. Signed or broadcast transactions remain tracked.
    Cancel {
        id: String,
    },
}

fn show(ctx: &Ctx, plan: &wallet_core::plans::TradePlan) -> Result<()> {
    if ctx.out.json() {
        ctx.out.emit("order", plan);
        return Ok(());
    }
    let value = orders::details(plan)?;
    println!("{}  {:?}  {:?}", plan.id, value.state, value.spec.mode);
    println!("{}", plan.reason);
    println!(
        "input {} atoms ({}) → at least {} atoms ({})",
        value.spec.input_atoms, value.spec.from, value.spec.minimum_output_atoms, value.spec.to
    );
    println!(
        "router {} · expires {} · attempts {}/{}",
        value.spec.router,
        value.spec.expires_at,
        value.attempts.len(),
        value.spec.max_attempts
    );
    println!("fee authorization consumed {} / {} QUAI atoms", value.fee_budget_used_atoms, value.spec.total_fee_budget_atoms);
    Ok(())
}

fn require_bounded_mode(mode: Mode, bounded: bool, yes: bool, policy: bool) -> Result<()> {
    if bounded && (mode != Mode::ExecuteOnce || !yes || !policy) {
        return Err(CoreError::Rejected("bounded execution requires an execute-once order, --yes and --authorization-policy".into()));
    }
    if yes && !bounded {
        return Err(CoreError::Rejected("order run needs interactive confirmation unless --bounded is explicitly selected".into()));
    }
    Ok(())
}

pub async fn run(ctx: &mut Ctx, args: OrderArgs) -> Result<()> {
    match args.command {
        OrderCmd::Create {
            from,
            to,
            amount,
            min_receive,
            account,
            max_fee,
            total_fee_budget,
            max_attempts,
            slippage,
            expires_in,
            execute_once,
        } => {
            if !(61..=30 * 86_400).contains(&expires_in) {
                return Err(CoreError::Invalid("expiry must be 61 seconds through 30 days".into()));
            }
            let mut session = ctx.session().await?;
            let plan = orders::create(
                &mut session,
                orders::Create {
                    account,
                    from,
                    to,
                    input: amount,
                    minimum_output: min_receive,
                    slippage_bps: slippage,
                    expires_at: wallet_core::registry::now() + u64::from(expires_in),
                    maximum_fee: max_fee,
                    total_fee_budget,
                    max_attempts,
                    mode: if execute_once { Mode::ExecuteOnce } else { Mode::Trigger },
                },
            )
            .await?;
            show(ctx, &plan)
        }
        OrderCmd::List => {
            let session = ctx.session().await?;
            let plans = orders::list(&session)?;
            if ctx.out.json() {
                ctx.out.emit("orders", &plans);
            } else {
                for plan in &plans {
                    show(ctx, plan)?;
                }
            }
            Ok(())
        }
        OrderCmd::Show { id } => {
            let session = ctx.session().await?;
            show(ctx, &orders::load(&session, &id)?)
        }
        OrderCmd::Observe { id, samples, interval } => {
            if !(1..=720).contains(&samples) || !(1..=60).contains(&interval) {
                return Err(CoreError::Invalid("samples must be 1..720 and interval 1..60 seconds".into()));
            }
            let mut session = ctx.session().await?;
            for index in 0..samples {
                session.track().await?;
                let observation = orders::observe(&mut session, &id).await?;
                if ctx.out.json() {
                    ctx.out.emit("order observe", &observation);
                } else {
                    println!("{}  {:?}  {}", observation.id, observation.state, observation.reason);
                }
                if observation.triggered
                    || matches!(
                        observation.state,
                        orders::State::Cancelled | orders::State::Complete | orders::State::Expired | orders::State::Stopped
                    )
                {
                    break;
                }
                if index + 1 < samples {
                    tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
                }
            }
            Ok(())
        }
        OrderCmd::Run { id, bounded } => {
            // Validate the explicit mode before asking for an unlock password.
            let view = ctx.session().await?;
            let plan = orders::load(&view, &id)?;
            require_bounded_mode(orders::details(&plan)?.spec.mode, bounded, ctx.global.yes, ctx.global.authorization_policy.is_some())?;
            drop(view);
            let mut session = ctx.unlocked().await?;
            session.track().await?;
            match orders::prepare(&mut session, &id).await? {
                Some(review) => {
                    let submitted = ctx.authorize(&mut session, review).await?;
                    ctx.print_submitted("order run", &submitted);
                    Ok(())
                }
                None => show(ctx, &orders::load(&session, &id)?),
            }
        }
        OrderCmd::Cancel { id } => {
            let mut session = ctx.session().await?;
            let plan = orders::cancel(&mut session, &id)?;
            show(ctx, &plan)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn automated_order_requires_all_explicit_opt_ins() {
        assert!(require_bounded_mode(Mode::Trigger, true, true, true).is_err());
        assert!(require_bounded_mode(Mode::ExecuteOnce, true, true, false).is_err());
        assert!(require_bounded_mode(Mode::ExecuteOnce, true, false, true).is_err());
        assert!(require_bounded_mode(Mode::ExecuteOnce, false, true, true).is_err());
        assert!(require_bounded_mode(Mode::ExecuteOnce, true, true, true).is_ok());
        assert!(require_bounded_mode(Mode::Trigger, false, false, false).is_ok());
    }
}
