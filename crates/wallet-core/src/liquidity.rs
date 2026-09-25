//! Providing liquidity to Quainance pools: what a position is worth, what a deposit would buy,
//! and what a withdrawal would return.
//!
//! The router is a stock UniswapV2 Router02, so the arithmetic here is the stock arithmetic. What
//! the wallet adds is the same discipline the swap path already has: the paired amount is derived
//! from reserves rather than typed, minimums come from the user's slippage, and the first deposit
//! into an empty pool is refused rather than silently letting the depositor set the price.

use crate::journal::OpKind;
use crate::amount;
use crate::markets::{Pool, PoolToken};
use quai_sdk::U256;
use serde::{Deserialize, Serialize};

/// Router functions for providing liquidity. The pinned router has all of these; `swap.rs` simply
/// never declared them.
pub const LIQUIDITY_ABI: &[&str] = &[
    "function addLiquidity(address tokenA, address tokenB, uint256 amountADesired, uint256 amountBDesired, uint256 amountAMin, uint256 amountBMin, address to, uint256 deadline) returns (uint256 amountA, uint256 amountB, uint256 liquidity)",
    "function addLiquidityETH(address token, uint256 amountTokenDesired, uint256 amountTokenMin, uint256 amountETHMin, address to, uint256 deadline) payable returns (uint256 amountToken, uint256 amountETH, uint256 liquidity)",
    "function removeLiquidity(address tokenA, address tokenB, uint256 liquidity, uint256 amountAMin, uint256 amountBMin, address to, uint256 deadline) returns (uint256 amountA, uint256 amountB)",
    "function removeLiquidityETH(address token, uint256 liquidity, uint256 amountTokenMin, uint256 amountETHMin, address to, uint256 deadline) returns (uint256 amountToken, uint256 amountETH)",
];

/// Pair functions the position reader needs beyond `markets::PAIR_READ_ABI`.
pub const LP_TOKEN_ABI: &[&str] =
    &["function totalSupply() view returns (uint256)", "function balanceOf(address owner) view returns (uint256)"];

/// A holding in one pool: what is in the wallet, what is staked, and what it redeems for.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct LpPosition {
    /// Pair contract (lowercase).
    pub pair: String,
    pub token0: PoolToken,
    pub token1: PoolToken,
    /// LP held in the account.
    #[serde(with = "crate::explorer::u256_string")]
    pub lp_wallet: U256,
    /// LP staked in the gauge named by `gauge`.
    #[serde(with = "crate::explorer::u256_string")]
    pub lp_staked: U256,
    /// Total LP in existence.
    #[serde(with = "crate::explorer::u256_string")]
    pub lp_total: U256,
    /// Redeemable token0 for `lp_wallet + lp_staked`.
    #[serde(with = "crate::explorer::u256_string")]
    pub amount0: U256,
    /// Redeemable token1.
    #[serde(with = "crate::explorer::u256_string")]
    pub amount1: U256,
    /// USD value of the position, when the pool is priced.
    pub usd: Option<f64>,
    /// Gauge pool id, when this pair is staked-eligible.
    pub pid: Option<u64>,
    /// Which gauge `pid` and `lp_staked` belong to.
    #[serde(default)]
    pub gauge: Option<crate::gauge::GaugeKind>,
    /// Exact gauge deployment for `pid`; required when several gauges enroll this pair.
    #[serde(default)]
    pub gauge_address: Option<String>,
}

impl LpPosition {
    /// Wallet plus staked.
    pub fn lp_total_held(&self) -> U256 {
        self.lp_wallet.saturating_add(self.lp_staked)
    }

    /// Share of the pool in basis points.
    pub fn share_bps(&self) -> u64 {
        share_bps(self.lp_total_held(), self.lp_total)
    }

    /// Nothing held here.
    pub fn is_empty(&self) -> bool {
        self.lp_total_held().is_zero()
    }

    /// `WQI/WQUAI`.
    pub fn name(&self) -> String {
        format!("{}/{}", self.token0.symbol, self.token1.symbol)
    }

    /// `0.42%`, or `<0.01%` for a position too small to show at two decimals.
    pub fn share_text(&self) -> String {
        let bps = self.share_bps();
        if bps == 0 && !self.is_empty() { "<0.01%".into() } else { format!("{:.2}%", bps as f64 / 100.0) }
    }

    /// `18.2 WQI + 2,214 WQUAI`.
    pub fn underlying_text(&self) -> String {
        format!(
            "{} {} + {} {}",
            amount::group_thousands(&amount::format_amount_short(self.amount0, self.token0.decimals, 4)),
            self.token0.symbol,
            amount::group_thousands(&amount::format_amount_short(self.amount1, self.token1.decimals, 4)),
            self.token1.symbol,
        )
    }
}

/// `held / total` in basis points, rounded down. Zero when the pool is empty.
pub fn share_bps(held: U256, total: U256) -> u64 {
    if total.is_zero() {
        return 0;
    }
    let scaled = amount::mul_div(held, U256::from(10_000u64), total).unwrap_or(U256::MAX);
    u64::try_from(scaled).unwrap_or(10_000).min(10_000)
}

/// What `liquidity` LP redeems for, at the pool's current reserves.
///
/// This is the pair's own arithmetic: a share of each reserve, rounded down.
pub fn redeemable(liquidity: U256, total_supply: U256, reserve0: U256, reserve1: U256) -> (U256, U256) {
    if total_supply.is_zero() {
        return (U256::ZERO, U256::ZERO);
    }
    (
        amount::mul_div(liquidity, reserve0, total_supply).unwrap_or(U256::ZERO),
        amount::mul_div(liquidity, reserve1, total_supply).unwrap_or(U256::ZERO),
    )
}

/// The paired amount a deposit needs, at the pool's ratio: `amount_a × reserve_b / reserve_a`.
///
/// The user types one side; this is the other. Typing both is how people donate to arbitrageurs.
pub fn pair_amount(amount_a: U256, reserve_a: U256, reserve_b: U256) -> U256 {
    if reserve_a.is_zero() {
        return U256::ZERO;
    }
    amount::mul_div(amount_a, reserve_b, reserve_a).unwrap_or(U256::ZERO)
}

/// LP minted for a deposit into a pool that already has reserves. UniswapV2 mints the *smaller*
/// of the two ratios, so an unbalanced deposit is penalised — which is why the paired amount is
/// derived rather than typed.
pub fn liquidity_minted(amount0: U256, amount1: U256, reserve0: U256, reserve1: U256, total_supply: U256) -> U256 {
    if total_supply.is_zero() || reserve0.is_zero() || reserve1.is_zero() {
        return U256::ZERO;
    }
    let by0 = amount::mul_div(amount0, total_supply, reserve0).unwrap_or(U256::ZERO);
    let by1 = amount::mul_div(amount1, total_supply, reserve1).unwrap_or(U256::ZERO);
    by0.min(by1)
}

/// `amount × (10000 − slippage) / 10000`, the floor the router is told to respect.
pub fn minimum(amount: U256, slippage_bps: u16) -> U256 {
    crate::swap::minimum_out(amount, slippage_bps)
}

/// `amount × (10000 + slippage) / 10000`: the same tolerance, upward.
pub fn ceiling(amount: U256, slippage_bps: u16) -> U256 {
    amount::mul_div(amount, U256::from(10_000u64 + u64::from(slippage_bps.min(10_000))), U256::from(10_000u64)).unwrap_or(U256::MAX)
}

/// Which side of a pool a typed deposit amount is denominated in.
///
/// One side is typed and the other is derived; which one is typed is the depositor's choice,
/// because the scarce token is the one that decides how large the deposit can be.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Side {
    #[default]
    Token0,
    Token1,
}

impl Side {
    /// Read a side from a token symbol. `None` keeps the pool's first token, the side the forms
    /// asked for before there was a choice.
    pub fn of(pool: &Pool, symbol: Option<&str>) -> Result<Side> {
        let Some(symbol) = symbol.map(str::trim).filter(|s| !s.is_empty()) else { return Ok(Side::Token0) };
        if symbol.eq_ignore_ascii_case(&pool.token0.symbol) {
            Ok(Side::Token0)
        } else if symbol.eq_ignore_ascii_case(&pool.token1.symbol) {
            Ok(Side::Token1)
        } else {
            Err(CoreError::Invalid(format!("{symbol} is not in this pool — deposit in {} or {}", pool.token0.symbol, pool.token1.symbol)))
        }
    }

    /// The token this side deposits.
    pub fn token(self, pool: &Pool) -> &PoolToken {
        match self {
            Side::Token0 => &pool.token0,
            Side::Token1 => &pool.token1,
        }
    }
}

/// A deposit, priced against the pool as it stands.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AddLiquidityQuote {
    pub pair: Option<String>,
    pub token0: PoolToken,
    pub token1: PoolToken,
    #[serde(with = "crate::explorer::u256_string")]
    pub amount0: U256,
    #[serde(with = "crate::explorer::u256_string")]
    pub amount1: U256,
    #[serde(with = "crate::explorer::u256_string")]
    pub amount0_min: U256,
    #[serde(with = "crate::explorer::u256_string")]
    pub amount1_min: U256,
    /// LP this deposit would mint.
    #[serde(with = "crate::explorer::u256_string")]
    pub liquidity: U256,
    /// Share of the pool afterwards, in basis points.
    pub share_after_bps: u64,
    /// The pool is empty, so this deposit sets the opening price.
    pub first_provider: bool,
    pub slippage_bps: u16,
    /// Which side the depositor typed; the other is the pool's answer and moves with its reserves.
    pub side: Side,
    /// Which exchange holds this pair. A router only serves its own factory's pairs, so the
    /// approval and the deposit have to name the same one the pool came from.
    #[serde(default)]
    pub venue: crate::markets::Venue,
    /// The router's current allowance on each side, when an owner was given.
    pub allowance0: Option<String>,
    pub allowance1: Option<String>,
    /// Whether each side still needs an approval before the deposit can be signed. `false` on a
    /// side already approved for enough: approving again would cost a fee for nothing.
    pub approval0_needed: bool,
    pub approval1_needed: bool,
    pub warnings: Vec<String>,
}

impl AddLiquidityQuote {
    /// Build from a pool's reserves and the amount the user typed on `side`; the other side is
    /// derived from the pool's ratio.
    pub fn build(
        pool: &Pool,
        reserve0: U256,
        reserve1: U256,
        total_supply: U256,
        amount: U256,
        side: Side,
        slippage_bps: u16,
    ) -> AddLiquidityQuote {
        let first_provider = total_supply.is_zero() || reserve0.is_zero() || reserve1.is_zero();
        let (amount0, amount1) = match side {
            Side::Token0 => (amount, pair_amount(amount, reserve0, reserve1)),
            Side::Token1 => (pair_amount(amount, reserve1, reserve0), amount),
        };
        let liquidity = liquidity_minted(amount0, amount1, reserve0, reserve1, total_supply);
        let mut warnings = Vec::new();
        if first_provider {
            warnings.push(
                "this pool is empty — the first deposit sets its opening price, and an arbitrageur can take the difference immediately"
                    .into(),
            );
        }
        AddLiquidityQuote {
            pair: (!pool.address.is_empty()).then(|| pool.address.clone()),
            token0: pool.token0.clone(),
            token1: pool.token1.clone(),
            amount0,
            amount1,
            amount0_min: minimum(amount0, slippage_bps),
            amount1_min: minimum(amount1, slippage_bps),
            liquidity,
            share_after_bps: share_bps(liquidity, total_supply.saturating_add(liquidity)),
            first_provider,
            slippage_bps,
            side,
            venue: pool.venue,
            allowance0: None,
            allowance1: None,
            // Unknown until an owner's allowances are read; `with_allowances` settles them.
            approval0_needed: true,
            approval1_needed: true,
            warnings,
        }
    }

    /// Record the router's allowances, which decide whether either side needs approving at all.
    pub fn with_allowances(mut self, allowance0: U256, allowance1: U256) -> Self {
        self.approval0_needed = allowance0 < self.amount0;
        self.approval1_needed = allowance1 < self.amount1;
        self.allowance0 = Some(allowance0.to_string());
        self.allowance1 = Some(allowance1.to_string());
        self
    }

    /// The allowance an approval should set for one side.
    ///
    /// Exactly the deposit on the side the depositor typed. On the side the pool derives, the
    /// same plus their slippage: that amount moves with every trade against the pool, and a cap
    /// that cannot absorb the movement is stale before the deposit is signed — which is how an
    /// approval goes through and the deposit still asks to approve again.
    pub fn approval_cap(&self, side1: bool) -> U256 {
        let (amount, derived) = if side1 { (self.amount1, self.side == Side::Token0) } else { (self.amount0, self.side == Side::Token1) };
        if derived { ceiling(amount, self.slippage_bps) } else { amount }
    }

    /// Sides still to approve, in the order they are asked for.
    pub fn approvals_needed(&self) -> Vec<bool> {
        [false, true].into_iter().filter(|s| if *s { self.approval1_needed } else { self.approval0_needed }).collect()
    }

    /// `50 WQI + 6,082.14 WQUAI`.
    pub fn deposit_text(&self) -> String {
        format!(
            "{} {} + {} {}",
            amount::format_amount(self.amount0, self.token0.decimals),
            self.token0.symbol,
            amount::group_thousands(&amount::format_amount_short(self.amount1, self.token1.decimals, 6)),
            self.token1.symbol,
        )
    }

    /// Share of the pool after the deposit.
    pub fn share_text(&self) -> String {
        if self.first_provider { "100% (first provider)".into() } else { format!("{:.2}%", self.share_after_bps as f64 / 100.0) }
    }
}

/// A withdrawal, priced against the pool as it stands.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RemoveLiquidityQuote {
    pub pair: String,
    pub token0: PoolToken,
    pub token1: PoolToken,
    /// LP burned.
    #[serde(with = "crate::explorer::u256_string")]
    pub liquidity: U256,
    #[serde(with = "crate::explorer::u256_string")]
    pub amount0: U256,
    #[serde(with = "crate::explorer::u256_string")]
    pub amount1: U256,
    #[serde(with = "crate::explorer::u256_string")]
    pub amount0_min: U256,
    #[serde(with = "crate::explorer::u256_string")]
    pub amount1_min: U256,
    /// LP that must be unstaked from the gauge first, when the wallet alone cannot cover it.
    #[serde(with = "crate::explorer::u256_string")]
    pub unstake_first: U256,
    pub slippage_bps: u16,
}

impl RemoveLiquidityQuote {
    /// Withdraw `percent` of a position.
    pub fn build(position: &LpPosition, reserve0: U256, reserve1: U256, percent: u8, slippage_bps: u16) -> RemoveLiquidityQuote {
        let percent = percent.min(100);
        let liquidity =
            amount::mul_div(position.lp_total_held(), U256::from(percent), U256::from(100u64)).expect("withdrawal percentage is bounded");
        let (amount0, amount1) = redeemable(liquidity, position.lp_total, reserve0, reserve1);
        RemoveLiquidityQuote {
            pair: position.pair.clone(),
            token0: position.token0.clone(),
            token1: position.token1.clone(),
            liquidity,
            amount0,
            amount1,
            amount0_min: minimum(amount0, slippage_bps),
            amount1_min: minimum(amount1, slippage_bps),
            // Staked LP is not in the account, so the router cannot burn it until it comes back.
            unstake_first: liquidity.saturating_sub(position.lp_wallet),
            slippage_bps,
        }
    }

    /// `18.2 WQI + 2,214 WQUAI`.
    pub fn receive_text(&self) -> String {
        format!(
            "{} {} + {} {}",
            amount::group_thousands(&amount::format_amount_short(self.amount0, self.token0.decimals, 6)),
            self.token0.symbol,
            amount::group_thousands(&amount::format_amount_short(self.amount1, self.token1.decimals, 6)),
            self.token1.symbol,
        )
    }
}

/// How far a position's value has drifted from simply holding the two tokens, in percent.
///
/// `ratio` is the price now over the price when the position opened. The closed form for a
/// constant-product pool is `2√r / (1 + r) − 1`, which is always ≤ 0. Stated concretely in a
/// review it is worth more than a paragraph of disclaimer.
pub fn impermanent_loss_pct(ratio: f64) -> f64 {
    if !ratio.is_finite() || ratio <= 0.0 {
        return 0.0;
    }
    (2.0 * ratio.sqrt() / (1.0 + ratio) - 1.0) * 100.0
}

// ------------------------------------------------------------------ reading positions on chain

use crate::chain::{addr, interface, uint};
use crate::data::{DataCtx, READ_CALLER, Trust};
use crate::error::{CoreError, Result};
use quai_sdk::BlockTag;
use quai_sdk::contracts::Contract;
use serde_json::json;

/// Every pool where these owners hold LP, wallet and staked together.
///
/// Discovery needs no indexer: the pool list is already loaded for Markets, and a `balanceOf` per
/// pair answers the rest. Pools where nothing is held are dropped, so the view shows holdings
/// rather than a catalogue.
pub async fn positions(
    ctx: &DataCtx,
    owners: &[String],
    pools: &[Pool],
    gauge: Option<&crate::gauge::GaugeView>,
    zone: Option<&crate::zone::ZoneView>,
) -> Vec<LpPosition> {
    let mut out = match batched_positions(ctx, owners, pools, gauge, zone).await {
        Some(found) => found,
        // No Multicall3 on this network: read pool by pool. Slower, but it needs no third contract.
        None => {
            let mut found = Vec::new();
            for pool in pools {
                if let Ok(rows) = positions_for_pool(ctx, owners, pool, gauge, zone).await {
                    found.extend(rows);
                }
            }
            found
        }
    };
    out.retain(|p| !p.is_empty());
    // Biggest first: a position's USD value when priced, else its share of the pool.
    out.sort_by(|a, b| {
        b.usd
            .unwrap_or(0.0)
            .total_cmp(&a.usd.unwrap_or(0.0))
            .then_with(|| b.share_bps().cmp(&a.share_bps()))
            .then_with(|| a.pair.cmp(&b.pair))
    });
    out
}

/// Every position in one round trip, or None when this network has no Multicall3.
///
/// Two calls per pool plus one per owner per pool: 21 pools and one account is 63 reads, which is
/// one `aggregate3` instead of 63 sequential ones.
async fn batched_positions(
    ctx: &DataCtx,
    owners: &[String],
    pools: &[Pool],
    gauge: Option<&crate::gauge::GaugeView>,
    zone: Option<&crate::zone::ZoneView>,
) -> Option<Vec<LpPosition>> {
    use crate::multicall::{Arg, Call, Multicall, word};
    let mc = Multicall::open(ctx).await?;
    let mut calls = Vec::with_capacity(pools.len() * (2 + owners.len()));
    for pool in pools {
        calls.push(Call::view(&pool.address, "getReserves()", &[]));
        calls.push(Call::view(&pool.address, "totalSupply()", &[]));
        for owner in owners {
            calls.push(Call::view(&pool.address, "balanceOf(address)", &[Arg::Addr(owner.clone())]));
        }
        // Staked LP, when this pair has a gauge pool and the gauge is readable.
        if let (Some(g), Some(pid)) = (gauge, gauge.and_then(|g| g.pid_for(&pool.address))) {
            for owner in owners {
                calls.push(Call::view(&g.address, "balanceOf(uint256,address)", &[Arg::Uint(U256::from(pid)), Arg::Addr(owner.clone())]));
            }
        }
    }
    let out = mc.try_all(&calls).await.ok()?;
    if out.len() != calls.len() || out.iter().any(|row| row.as_ref().is_none_or(|data| data.len() < 32)) {
        return None;
    }
    let mut found = Vec::new();
    let mut i = 0;
    let take = |out: &[Option<Vec<u8>>], i: &mut usize| -> U256 {
        let v = out.get(*i).and_then(Option::as_ref).map_or(U256::ZERO, |d| word(d, 0));
        *i += 1;
        v
    };
    for pool in pools {
        let reserves = out.get(i)?.as_ref()?;
        if reserves.len() < 96 {
            return None;
        }
        let reserves = (word(reserves, 0), word(reserves, 1));
        i += 1;
        let total = take(&out, &mut i);
        let mut wallet = U256::ZERO;
        for _ in owners {
            wallet = wallet.saturating_add(take(&out, &mut i));
        }
        let mut core_staked = U256::ZERO;
        if gauge.and_then(|g| g.pid_for(&pool.address)).is_some() {
            for _ in owners {
                core_staked = core_staked.checked_add(take(&out, &mut i))?;
            }
        }
        found.extend(positions_from_balances(pool, reserves, wallet, core_staked, total, gauge, zone)?);
    }
    Some(found)
}

/// Each deployment is a separate position. Wallet LP appears once, so summing rows cannot
/// double-count spendable funds; no stake or reward pid crosses gauge boundaries.
#[allow(clippy::too_many_arguments)]
fn positions_from_balances(
    pool: &Pool,
    reserves: (U256, U256),
    wallet: U256,
    core_staked: U256,
    total: U256,
    core: Option<&crate::gauge::GaugeView>,
    zone: Option<&crate::zone::ZoneView>,
) -> Option<Vec<LpPosition>> {
    use crate::gauge::GaugeKind;
    let mut targets = Vec::new();
    if let Some(g) = core
        && let Some(pid) = g.pid_for(&pool.address)
    {
        targets.push((g.address.clone(), pid, GaugeKind::Core, core_staked));
    }
    if let Some(z) = zone {
        targets.extend(
            z.pools
                .iter()
                .filter(|p| p.lp_token.eq_ignore_ascii_case(&pool.address))
                .map(|p| (p.gauge.clone(), p.pid, GaugeKind::Zone, p.staked)),
        );
    }
    if targets.is_empty() {
        return Some(vec![build_position(pool, reserves, wallet, U256::ZERO, total, None, None)?]);
    }
    let mut rows = Vec::with_capacity(targets.len());
    for (i, (address, pid, kind, staked)) in targets.into_iter().enumerate() {
        let mut row = build_position(pool, reserves, if i == 0 { wallet } else { U256::ZERO }, staked, total, Some(pid), Some(kind))?;
        row.gauge_address = Some(address);
        rows.push(row);
    }
    Some(rows)
}

/// Assemble a position from the four numbers that describe it. None when the pool's own reserves
/// cannot be parsed, which would make the redeemable amounts meaningless.
#[allow(clippy::too_many_arguments)]
fn build_position(
    pool: &Pool,
    reserves: (U256, U256),
    wallet: U256,
    staked: U256,
    total: U256,
    pid: Option<u64>,
    gauge: Option<crate::gauge::GaugeKind>,
) -> Option<LpPosition> {
    let held = wallet.checked_add(staked)?;
    if held > total {
        return None;
    }
    let (reserve0, reserve1) = reserves;
    let (amount0, amount1) = redeemable(held, total, reserve0, reserve1);
    Some(LpPosition {
        pair: pool.address.clone(),
        token0: pool.token0.clone(),
        token1: pool.token1.clone(),
        lp_wallet: wallet,
        lp_staked: staked,
        lp_total: total,
        amount0,
        amount1,
        // The position is worth its share of the pool's own TVL, which the pool already carries.
        usd: pool.tvl_usd.zip(amount::ratio(held, total)).map(|(tvl, share)| tvl * share),
        pid,
        gauge,
        gauge_address: None,
    })
}

/// All deployment-specific positions in one pair for these owners.
pub async fn positions_for_pool(
    ctx: &DataCtx,
    owners: &[String],
    pool: &Pool,
    gauge: Option<&crate::gauge::GaugeView>,
    zone: Option<&crate::zone::ZoneView>,
) -> Result<Vec<LpPosition>> {
    let pair = addr(&pool.address)?;
    let lp = Contract::new(pair, interface(LP_TOKEN_ABI)?, &ctx.node.provider);
    let caller = addr(READ_CALLER)?;
    let (reserve0, reserve1, total) = pair_state(ctx, pool).await?;
    let reserves = (reserve0, reserve1);
    let mut wallet = U256::ZERO;
    for owner in owners {
        wallet = wallet.saturating_add(uint(&lp.call(caller, "balanceOf", &[json!(owner)], BlockTag::Latest).await?, 0));
    }
    let core_staked = match gauge.and_then(|g| g.pid_for(&pool.address).map(|pid| (g, pid))) {
        Some((g, pid)) => g.staked_by(ctx, pid, owners).await?,
        None => U256::ZERO,
    };
    positions_from_balances(pool, reserves, wallet, core_staked, total, gauge, zone)
        .ok_or_else(|| CoreError::Invalid(format!("pool {} has inconsistent LP balances or supply", pool.address)))
}

/// One unambiguous pool position. Call `positions_for_pool` when displaying multiple gauges.
pub async fn position(
    ctx: &DataCtx,
    owners: &[String],
    pool: &Pool,
    gauge: Option<&crate::gauge::GaugeView>,
    zone: Option<&crate::zone::ZoneView>,
) -> Result<LpPosition> {
    let mut rows = positions_for_pool(ctx, owners, pool, gauge, zone).await?;
    if rows.len() != 1 {
        return Err(CoreError::Invalid("pair belongs to multiple gauges; display each deployment with positions_for_pool".into()));
    }
    Ok(rows.remove(0))
}

// ------------------------------------------------------------------------ providing liquidity

use crate::session::Session;

use crate::tx::{AccountRequest, Review, field};

/// Resolve an LP identity from the selected pair itself, then authenticate its membership in
/// the pinned factory. Explorer rows never supply executable token addresses or decimals.
async fn pool_by_address(ctx: &DataCtx, pair: &str) -> Result<Pool> {
    pool_by_address_at(ctx, pair, BlockTag::Latest).await
}

async fn pool_by_address_at(ctx: &DataCtx, pair: &str, block: BlockTag) -> Result<Pool> {
    let address = addr(pair)?;
    let caller = addr(READ_CALLER)?;
    let contract = Contract::new(
        address,
        interface(&[
            "function token0() view returns (address)",
            "function token1() view returns (address)",
            "function factory() view returns (address)",
        ])?,
        &ctx.node.provider,
    );
    let (a, b, f) = futures::future::join3(
        contract.call(caller, "token0", &[], block),
        contract.call(caller, "token1", &[], block),
        contract.call(caller, "factory", &[], block),
    )
    .await;
    let token0 = crate::chain::address_at(&a?, 0);
    let token1 = crate::chain::address_at(&b?, 0);
    let factory = crate::chain::address_at(&f?, 0);
    let a = addr(&token0)?;
    let b = addr(&token1)?;
    if a == b || crate::chain::is_zero_address(&token0) || crate::chain::is_zero_address(&token1) {
        return Err(CoreError::Rejected("the pair returned invalid token identities".into()));
    }
    let venue =
        [crate::markets::Venue::Main, crate::markets::Venue::LaunchAmm, crate::markets::Venue::Legacy, crate::markets::Venue::HartiiAmm]
            .into_iter()
            .find(|v| crate::swap::venue_pins(&ctx.network, *v).is_some_and(|(_, pin)| pin.address.eq_ignore_ascii_case(&factory)))
            .ok_or_else(|| CoreError::Rejected("the pair does not belong to a configured factory".into()))?;
    crate::swap::verified_pool_router(ctx, venue).await?;
    let factory_contract = Contract::new(
        addr(&factory)?,
        interface(&["function getPair(address tokenA, address tokenB) view returns (address)"])?,
        &ctx.node.provider,
    );
    let member = factory_contract.call(caller, "getPair", &[json!(token0), json!(token1)], block).await?;
    verify_pair_membership(&address.to_string(), &crate::chain::address_at(&member, 0))?;
    let metadata = async |address: String| -> Result<PoolToken> {
        let (symbol, _, decimals) = ctx.erc20_metadata_at(&address, READ_CALLER, block).await?;
        Ok(PoolToken { address, symbol, decimals })
    };
    let (token0, token1) = futures::future::try_join(metadata(token0), metadata(token1)).await?;
    Ok(Pool { address: address.to_string().to_lowercase(), token0, token1, venue, ..Pool::default() })
}

fn validate_percent(percent: u8) -> Result<()> {
    if !(1..=100).contains(&percent) {
        return Err(CoreError::Invalid("withdrawal percentage must be between 1 and 100".into()));
    }
    Ok(())
}

fn verify_pair_membership(requested: &str, factory_pair: &str) -> Result<()> {
    if !requested.eq_ignore_ascii_case(factory_pair) || crate::chain::is_zero_address(factory_pair) {
        return Err(CoreError::Rejected("the pinned factory does not authenticate the selected pair and its tokens".into()));
    }
    Ok(())
}

/// On-chain reserves and supply for a pair, which the quotes are built from. The pool list's
/// figures are display data; anything that reaches a review is read here.
async fn pair_state(ctx: &DataCtx, pool: &Pool) -> Result<(U256, U256, U256)> {
    let anchor = ReadAnchor::capture(ctx).await?;
    let state = pair_state_at(ctx, pool, anchor.block()).await?;
    anchor.prove_pair(ctx, &pool.address, state, None).await?;
    anchor.check(ctx).await?;
    Ok(state)
}

/// A UniswapV2 pair's `totalSupply` and `balanceOf` mapping: slots 0 and 1 on every venue here.
const PAIR_SUPPLY_SLOT: u64 = 0;
const PAIR_BALANCE_SLOT: u64 = 1;

struct ReadAnchor {
    height: u64,
    hash: String,
    /// On a review, the node's state anchor: every read is made at its block, and what can be is
    /// proven there as well, so the two must agree exactly.
    proven: Option<crate::anchor::Anchored>,
}
impl ReadAnchor {
    async fn capture(ctx: &DataCtx) -> Result<Self> {
        ctx.online()?;
        // A network whose exchanges carry no bytecode pin is a custom or development chain; its
        // reads stay as they were, since nothing here says its headers are go-quai's.
        let pinned = ctx.network.ecosystem.quainance_factory.as_ref().is_some_and(|f| f.code_hash.is_some());
        if !ctx.trust.may_cache()
            && pinned
            && let Some(anchored) = crate::anchor::review_anchor(&ctx.node, &ctx.network, "the pool").await?
        {
            let block = anchored.anchor.block;
            return Ok(Self { height: block.number, hash: block.hash.to_string(), proven: Some(anchored) });
        }
        let head = ctx
            .node
            .provider
            .latest_header(crate::network::ZONE)
            .await?
            .ok_or_else(|| CoreError::Network("missing LP observation header".into()))?;
        Ok(Self { height: head.number, hash: head.hash.to_string(), proven: None })
    }

    /// On a review, hold what was read at this block to what the pair's storage proves there:
    /// `totalSupply` (slot 0), the packed reserves (slot 8) and, for a position, the owner's LP
    /// balance (`balanceOf`, a mapping at slot 1). Any difference is the node's answer and its own
    /// state disagreeing.
    async fn prove_pair(&self, ctx: &DataCtx, pair: &str, state: (U256, U256, U256), holding: Option<(&str, U256)>) -> Result<()> {
        let Some(anchored) = &self.proven else { return Ok(()) };
        let pair = addr(pair)?;
        let mut slots = vec![crate::anchor::slot(PAIR_SUPPLY_SLOT), crate::anchor::slot(crate::swap::PAIR_RESERVES_SLOT)];
        if let Some((owner, _)) = holding {
            slots.push(crate::anchor::mapping_field_slot(addr(owner)?, PAIR_BALANCE_SLOT, 0));
        }
        let proven = crate::anchor::prove_at(&ctx.node, &ctx.network, anchored, &[(pair, &slots)], "the pool").await?;
        let value = |i: usize| proven[0].storage_value(slots[i]).unwrap_or_default();
        let (r0, r1) = crate::swap::unpack_reserves(value(1));
        let agrees = value(0) == state.2 && (r0, r1) == (state.0, state.1) && holding.is_none_or(|(_, held)| value(2) == held);
        if !agrees {
            return Err(CoreError::Rejected(
                "the node's reading of this pool does not match the pool's own state at the same block; refusing to review it".into(),
            ));
        }
        Ok(())
    }
    fn block(&self) -> BlockTag {
        BlockTag::Number(U256::from(self.height))
    }
    async fn check(&self, ctx: &DataCtx) -> Result<()> {
        if ctx.node.provider.header_at(crate::network::ZONE, self.height).await?.is_none_or(|head| head.hash.to_string() != self.hash) {
            return Err(CoreError::Rejected("LP observation was reorganized; request a fresh quote".into()));
        }
        Ok(())
    }
}

async fn pair_state_at(ctx: &DataCtx, pool: &Pool, block: BlockTag) -> Result<(U256, U256, U256)> {
    let pair = addr(&pool.address)?;
    let caller = addr(READ_CALLER)?;
    let lp = Contract::new(pair, interface(LP_TOKEN_ABI)?, &ctx.node.provider);
    let total = uint(&lp.call(caller, "totalSupply", &[], block).await?, 0);
    let reads = Contract::new(
        pair,
        interface(&["function getReserves() view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast)"])?,
        &ctx.node.provider,
    );
    let r = reads.call(caller, "getReserves", &[], block).await?;
    Ok((uint(&r, 0), uint(&r, 1), total))
}

async fn position_for_review(ctx: &DataCtx, owner: &str, pool: &Pool) -> Result<(LpPosition, U256, U256)> {
    let anchor = ReadAnchor::capture(ctx).await?;
    let authenticated = pool_by_address_at(ctx, &pool.address, anchor.block()).await?;
    if authenticated.token0 != pool.token0 || authenticated.token1 != pool.token1 || authenticated.venue != pool.venue {
        return Err(CoreError::Rejected("LP identity changed while reading the position; refresh before continuing".into()));
    }
    let (r0, r1, total) = pair_state_at(ctx, &authenticated, anchor.block()).await?;
    let erc = quai_sdk::contracts::Erc20::new(addr(&pool.address)?, &ctx.node.provider)?;
    let wallet = erc.balance_of(addr(READ_CALLER)?, addr(owner)?, anchor.block()).await?;
    let staked = crate::gauge::staked_for_pair_at(ctx, owner, &pool.address, anchor.block()).await?;
    anchor.prove_pair(ctx, &pool.address, (r0, r1, total), Some((owner, wallet))).await?;
    anchor.check(ctx).await?;
    let position = build_position(&authenticated, (r0, r1), wallet, staked, total, None, None)
        .ok_or_else(|| CoreError::Invalid("LP position cannot be valued".into()))?;
    Ok((position, r0, r1))
}

/// Quote a deposit from read-only data: the user gives one side, the pool decides the other.
///
/// `token` names the side the amount is in, so someone short of one token can size the deposit by
/// it; `None` means the pool's first token. This holds no keys, so a screen can keep it current
/// as the user types.
pub async fn quote(
    ctx: &DataCtx,
    pair: &str,
    value: &str,
    token: Option<&str>,
    slippage_bps: u16,
    owner: Option<&str>,
) -> Result<AddLiquidityQuote> {
    crate::swap::validate_slippage(slippage_bps)?;
    let anchor = ReadAnchor::capture(ctx).await?;
    let pool = pool_by_address_at(ctx, pair, anchor.block()).await?;
    let (r0, r1, total) = pair_state_at(ctx, &pool, anchor.block()).await?;
    anchor.prove_pair(ctx, &pool.address, (r0, r1, total), None).await?;
    let side = Side::of(&pool, token)?;
    let typed = amount::parse_amount(value, side.token(&pool).decimals)?;
    if typed.is_zero() {
        return Err(CoreError::Invalid("amount must be greater than zero".into()));
    }
    let quote = AddLiquidityQuote::build(&pool, r0, r1, total, typed, side, slippage_bps);
    if !quote.first_provider {
        crate::swap::require_minimum(quote.amount0_min)?;
        crate::swap::require_minimum(quote.amount1_min)?;
        crate::swap::require_minimum(quote.liquidity)?;
    }
    // What the router may already move on this owner's behalf: a side approved for enough needs
    // no second approval, and the screen says so before a fee is spent finding out.
    let Some(owner) = owner else {
        anchor.check(ctx).await?;
        return Ok(quote);
    };
    let (owner, router) = (addr(owner)?, crate::swap::verified_pool_router(ctx, pool.venue).await?);
    let allowance = async |token: &PoolToken| -> Result<U256> {
        let erc = quai_sdk::contracts::Erc20::new(addr(&token.address)?, &ctx.node.provider)?;
        Ok(erc.allowance(addr(READ_CALLER)?, owner, router, anchor.block()).await?)
    };
    let quote = quote.with_allowances(allowance(&pool.token0).await?, allowance(&pool.token1).await?);
    anchor.check(ctx).await?;
    Ok(quote)
}

impl Session {
    /// Owner-scoped withdrawal preview; performs no signing, change allocation or reservation.
    pub async fn remove_liquidity_quote_for(
        &mut self,
        account: Option<&str>,
        pair: &str,
        percent: u8,
        slippage_bps: u16,
    ) -> Result<RemoveLiquidityQuote> {
        validate_percent(percent)?;
        crate::swap::validate_slippage(slippage_bps)?;
        let owner = self.account(account)?.address;
        let ctx = self.data_ctx_at(Trust::FirstHand)?;
        let pool = pool_by_address(&ctx, pair).await?;
        let (position, r0, r1) = position_for_review(&ctx, &owner, &pool).await?;
        Ok(RemoveLiquidityQuote::build(&position, r0, r1, percent, slippage_bps))
    }
    /// Quote a deposit against this session's data context.
    ///
    /// `trust` says whether the numbers are going on screen or into a review; a review's quote
    /// reads the pair's reserves, the router's pins and the owner's allowances first-hand.
    pub async fn add_liquidity_quote(
        &mut self,
        pair: &str,
        value: &str,
        token: Option<&str>,
        slippage_bps: u16,
        trust: Trust,
    ) -> Result<AddLiquidityQuote> {
        self.add_liquidity_quote_for(None, pair, value, token, slippage_bps, trust).await
    }

    /// A deposit quote scoped to the account that will sign its approvals and deposit.
    pub async fn add_liquidity_quote_for(
        &mut self,
        account: Option<&str>,
        pair: &str,
        value: &str,
        token: Option<&str>,
        slippage_bps: u16,
        trust: Trust,
    ) -> Result<AddLiquidityQuote> {
        let owner = self.account(account)?.address;
        let ctx = self.data_ctx_at(trust)?;
        quote(&ctx, pair, value, token, slippage_bps, Some(&owner)).await
    }

    /// Review the exact approval one side of a deposit needs. `token` says which side `value` is
    /// in; `approve1` picks which side this approval is for.
    pub async fn review_add_approval(
        &mut self,
        account: Option<&str>,
        pair: &str,
        value: &str,
        token: Option<&str>,
        slippage_bps: u16,
        approve1: bool,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        let quote = self.add_liquidity_quote_for(account, pair, value, token, slippage_bps, Trust::FirstHand).await?;
        let (approved, needed) = if approve1 { (quote.token1.clone(), quote.amount1) } else { (quote.token0.clone(), quote.amount0) };
        // The cap covers the deposit and, on the derived side, the movement the depositor
        // already tolerates — so one approval survives until the deposit is signed.
        let atoms = quote.approval_cap(approve1);
        let router = crate::swap::verified_pool_router(&self.data_ctx_at(Trust::FirstHand)?, quote.venue).await?;
        let from = self.account(account)?;
        let owner = addr(&from.address)?;
        let erc = quai_sdk::contracts::Erc20::new(addr(&approved.address)?, &self.node.provider)?;
        if erc.allowance(owner, owner, router, BlockTag::Latest).await? >= needed {
            return Err(CoreError::Rejected(format!("{} is already approved for this deposit", approved.symbol)));
        }
        let balance = erc.balance_of(owner, owner, BlockTag::Latest).await?;
        if balance < needed {
            return Err(CoreError::Insufficient(format!(
                "{} balance is {}; the deposit needs {}",
                approved.symbol,
                amount::format_amount(balance, approved.decimals),
                amount::format_amount(needed, approved.decimals)
            )));
        }
        let atoms = self.bounded_allowance(&approved.address, owner, router, atoms).await?;
        let call = erc.approve(router, atoms)?;
        let call = crate::data::with_access_list(&self.node.provider, owner, call).await?;
        let step = if approve1 { "step 2 of 3" } else { "step 1 of 3" };
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: OpKind::Approve,
            title: format!("Approve {} for liquidity ({step})", approved.symbol),
            asset: approved.symbol.clone(),
            amount: atoms,
            decimals: approved.decimals,
            counterparty: router.to_string(),
            fields: vec![
                field("Token contract", approved.address.clone()),
                field("Spender", crate::swap::router_field(&self.network, &self.node, quote.venue, &router.to_string())),
                field("Allowance", format!("exactly {} {}", amount::format_amount(atoms, approved.decimals), approved.symbol)),
            ],
            warnings: quote.warnings.clone(),
            detail: serde_json::json!({"token": approved.address, "purpose": "add_liquidity", "spender": router.to_string()}).into(),
            max_gas: 120_000,
            max_fee: self.parse_fee_cap(max_fee, amount::QUAI_DECIMALS)?,
        })
        .await
    }

    /// Review a deposit. Both sides must already be approved.
    pub async fn review_add_liquidity(
        &mut self,
        account: Option<&str>,
        pair: &str,
        value: &str,
        token: Option<&str>,
        slippage_bps: u16,
        deadline_minutes: u32,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        crate::swap::validate_deadline(deadline_minutes)?;
        let quote = self.add_liquidity_quote_for(account, pair, value, token, slippage_bps, Trust::FirstHand).await?;
        // An empty pool lets the depositor set the opening price outright, and an arbitrageur
        // takes the difference on the next block. Refuse rather than warn.
        if quote.first_provider {
            return Err(CoreError::Rejected(
                "this pool is empty — the first deposit sets its price and is immediately arbitraged; add liquidity to a pool with reserves"
                    .into(),
            ));
        }
        let router = crate::swap::verified_pool_router(&self.data_ctx_at(Trust::FirstHand)?, quote.venue).await?;
        let from = self.account(account)?;
        let owner = addr(&from.address)?;
        // Both approvals, checked before the review rather than surfaced as a revert.
        for (token, atoms) in [(&quote.token0, quote.amount0), (&quote.token1, quote.amount1)] {
            let erc = quai_sdk::contracts::Erc20::new(addr(&token.address)?, &self.node.provider)?;
            if erc.balance_of(owner, owner, BlockTag::Latest).await? < atoms {
                return Err(CoreError::Insufficient(format!("{} balance is too low for this deposit", token.symbol)));
            }
            if erc.allowance(owner, owner, router, BlockTag::Latest).await? < atoms {
                return Err(crate::error::approval_needed(
                    &token.address,
                    format!("approve {} {} for the router first", amount::format_amount(atoms, token.decimals), token.symbol),
                ));
            }
        }
        let contract = Contract::new(router, interface(LIQUIDITY_ABI)?, &self.node.provider);
        let deadline = crate::registry::now() + u64::from(deadline_minutes) * 60;
        let call = contract.prepare(
            "addLiquidity",
            &[
                json!(quote.token0.address),
                json!(quote.token1.address),
                json!(quote.amount0.to_string()),
                json!(quote.amount1.to_string()),
                json!(quote.amount0_min.to_string()),
                json!(quote.amount1_min.to_string()),
                json!(from.address),
                json!(deadline.to_string()),
            ],
            U256::ZERO,
        )?;
        let call = crate::data::with_access_list(&self.node.provider, owner, call).await?;
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: OpKind::AddLiquidity,
            title: format!("Add liquidity to {}/{}", quote.token0.symbol, quote.token1.symbol),
            asset: quote.token0.symbol.clone(),
            amount: quote.amount0,
            decimals: quote.token0.decimals,
            counterparty: router.to_string(),
            fields: vec![
                field("Pool", pair.to_string()),
                field("You deposit", quote.deposit_text()),
                field(
                    "Minimums",
                    format!(
                        "{} {} · {} {}  (slippage {:.2}%)",
                        amount::format_amount_short(quote.amount0_min, quote.token0.decimals, 6),
                        quote.token0.symbol,
                        amount::format_amount_short(quote.amount1_min, quote.token1.decimals, 6),
                        quote.token1.symbol,
                        f64::from(slippage_bps) / 100.0
                    ),
                ),
                field("Pool share after", quote.share_text()),
                field("Deadline", format!("{deadline_minutes} min")),
                field(
                    "Impermanent loss",
                    "if the two prices diverge, this position is worth less than simply holding both tokens: a 2× move costs about 5.7%, a 4× move about 20%",
                ),
            ],
            warnings: quote.warnings.clone(),
            detail: serde_json::json!({"expires_at": deadline,
                "pair": pair,
                "amount0": quote.amount0.to_string(),
                "amount1": quote.amount1.to_string(),
                "liquidity": quote.liquidity.to_string(),
                "financial_effects": [
                    {"direction":"out", "asset":quote.token0.symbol, "token":quote.token0.address, "decimals":quote.token0.decimals, "amount":quote.amount0.to_string(), "estimated":false, "note":"deposit"},
                    {"direction":"out", "asset":quote.token1.symbol, "token":quote.token1.address, "decimals":quote.token1.decimals, "amount":quote.amount1.to_string(), "estimated":false, "note":"deposit"},
                    {"direction":"in", "asset":"LP", "token":pair, "decimals":18, "amount":quote.liquidity.to_string(), "estimated":true, "note":"estimated minted liquidity"}
                ],
            }).into(),
            max_gas: 400_000,
            max_fee: self.parse_fee_cap(max_fee, amount::QUAI_DECIMALS)?,
        })
        .await
    }

    /// Review withdrawing a percentage of a position.
    pub async fn review_remove_liquidity(
        &mut self,
        account: Option<&str>,
        pair: &str,
        percent: u8,
        slippage_bps: u16,
        deadline_minutes: u32,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        validate_percent(percent)?;
        let from = self.account(account)?;
        let ctx = self.data_ctx_at(Trust::FirstHand)?;
        let pool = pool_by_address(&ctx, pair).await?;
        let (position, r0, r1) = position_for_review(&ctx, &from.address, &pool).await?;
        if position.is_empty() {
            return Err(CoreError::NotFound("no liquidity in this pool".into()));
        }
        crate::swap::validate_slippage(slippage_bps)?;
        crate::swap::validate_deadline(deadline_minutes)?;
        let quote = RemoveLiquidityQuote::build(&position, r0, r1, percent, slippage_bps);
        crate::swap::require_minimum(quote.amount0_min)?;
        crate::swap::require_minimum(quote.amount1_min)?;
        if quote.liquidity.is_zero() {
            return Err(CoreError::Invalid("that is none of the position".into()));
        }
        if !quote.unstake_first.is_zero() {
            return Err(CoreError::Rejected(format!(
                "unstake {} LP from the gauge first — staked LP is not in the account for the router to burn",
                amount::format_amount_short(quote.unstake_first, 18, 6)
            )));
        }
        let router = crate::swap::verified_pool_router(&ctx, pool.venue).await?;
        let from = self.account(account)?;
        let owner = addr(&from.address)?;
        let erc = quai_sdk::contracts::Erc20::new(addr(pair)?, &self.node.provider)?;
        if erc.allowance(owner, owner, router, BlockTag::Latest).await? < quote.liquidity {
            return Err(crate::error::approval_needed(pair, "approve the LP token for the router first (step 1 of 2)"));
        }
        let contract = Contract::new(router, interface(LIQUIDITY_ABI)?, &self.node.provider);
        let deadline = crate::registry::now() + u64::from(deadline_minutes) * 60;
        let call = contract.prepare(
            "removeLiquidity",
            &[
                json!(quote.token0.address),
                json!(quote.token1.address),
                json!(quote.liquidity.to_string()),
                json!(quote.amount0_min.to_string()),
                json!(quote.amount1_min.to_string()),
                json!(from.address),
                json!(deadline.to_string()),
            ],
            U256::ZERO,
        )?;
        let call = crate::data::with_access_list(&self.node.provider, owner, call).await?;
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: OpKind::RemoveLiquidity,
            title: format!("Remove {percent}% of {}/{}", quote.token0.symbol, quote.token1.symbol),
            asset: "LP".into(),
            amount: quote.liquidity,
            decimals: 18,
            counterparty: router.to_string(),
            fields: vec![
                field("Pool", pair.to_string()),
                field("Burning", format!("{} LP ({percent}% of your position)", amount::format_amount_short(quote.liquidity, 18, 6))),
                field("You receive", format!("≈ {}", quote.receive_text())),
                field(
                    "Pool output bounds",
                    format!(
                        "{} {} · {} {}  (slippage {:.2}%)",
                        amount::format_amount_short(quote.amount0_min, quote.token0.decimals, 6),
                        quote.token0.symbol,
                        amount::format_amount_short(quote.amount1_min, quote.token1.decimals, 6),
                        quote.token1.symbol,
                        f64::from(slippage_bps) / 100.0
                    ),
                ),
                field("Deadline", format!("{deadline_minutes} min")),
            ],
            warnings: vec!["The router bounds gross pool withdrawals. Transfer-tax or rebasing tokens can deliver less to your account; recipient credit is not guaranteed.".into()],
            detail: serde_json::json!({"expires_at": deadline, "pair": pair, "liquidity": quote.liquidity.to_string(), "percent": percent,
                "token0": quote.token0, "token1": quote.token1,
                "amount0": quote.amount0.to_string(), "amount1": quote.amount1.to_string(),
                "amount0_min": quote.amount0_min.to_string(), "amount1_min": quote.amount1_min.to_string(),
                "financial_effects": [
                    {"direction":"out", "asset":"LP", "token":pair, "decimals":18, "amount":quote.liquidity.to_string(), "estimated":false, "note":"burned liquidity"},
                    {"direction":"in", "asset":quote.token0.symbol, "token":quote.token0.address, "decimals":quote.token0.decimals, "amount":quote.amount0.to_string(), "estimated":true, "note":"estimated recipient credit; pool bound is before token transfer fees"},
                    {"direction":"in", "asset":quote.token1.symbol, "token":quote.token1.address, "decimals":quote.token1.decimals, "amount":quote.amount1.to_string(), "estimated":true, "note":"estimated recipient credit; pool bound is before token transfer fees"}
                ]}).into(),
            max_gas: 400_000,
            max_fee: self.parse_fee_cap(max_fee, amount::QUAI_DECIMALS)?,
        })
        .await
    }

    /// Review the exact LP approval a withdrawal needs (step 1 of 2).
    pub async fn review_remove_approval(
        &mut self,
        account: Option<&str>,
        pair: &str,
        percent: u8,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        validate_percent(percent)?;
        let from = self.account(account)?;
        let ctx = self.data_ctx_at(Trust::FirstHand)?;
        let pool = pool_by_address(&ctx, pair).await?;
        let (position, _, _) = position_for_review(&ctx, &from.address, &pool).await?;
        let liquidity = amount::mul_div(position.lp_total_held(), U256::from(percent), U256::from(100u64)).expect("percentage is bounded");
        if liquidity.is_zero() {
            return Err(CoreError::Invalid("that is none of the position".into()));
        }
        if liquidity > position.lp_wallet {
            return Err(CoreError::Rejected("unstake the required LP before approving its withdrawal".into()));
        }
        let router = crate::swap::verified_pool_router(&ctx, pool.venue).await?;
        let from = self.account(account)?;
        let owner = addr(&from.address)?;
        let liquidity = self.bounded_allowance(pair, owner, router, liquidity).await?;
        let call = quai_sdk::contracts::Erc20::new(addr(pair)?, &self.node.provider)?.approve(router, liquidity)?;
        let call = crate::data::with_access_list(&self.node.provider, owner, call).await?;
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: OpKind::Approve,
            title: "Approve LP for withdrawal (step 1 of 2)".into(),
            asset: "LP".into(),
            amount: liquidity,
            decimals: 18,
            counterparty: router.to_string(),
            fields: vec![
                field("LP token", pair.to_string()),
                field("Spender", crate::swap::router_field(&self.network, &self.node, pool.venue, &router.to_string())),
                field("Allowance", format!("exactly {} LP", amount::format_amount_short(liquidity, 18, 6))),
            ],
            warnings: vec![],
            detail: serde_json::json!({"token": pair, "purpose": "remove_liquidity", "spender": router.to_string()}).into(),
            max_gas: 120_000,
            max_fee: self.parse_fee_cap(max_fee, amount::QUAI_DECIMALS)?,
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e18(n: u128) -> U256 {
        U256::from(n) * U256::from(10u128.pow(18))
    }

    fn token(symbol: &str, decimals: u8) -> PoolToken {
        PoolToken { address: format!("0x00{symbol}"), symbol: symbol.into(), decimals }
    }

    fn pool() -> Pool {
        Pool {
            address: "0x00602f12ea0491f02865aa6c418815319e2a645b".into(),
            token0: token("WQI", 18),
            token1: token("WQUAI", 18),
            reserve0: 33_798.35,
            reserve1: 4_109_058.63,
            tvl_usd: Some(67_150.0),
            volume_24h_usd: None,

            ..Default::default()
        }
    }

    #[test]
    fn the_paired_amount_follows_the_pool_ratio() {
        // 33,798 WQI : 4,109,058 WQUAI ≈ 121.6 WQUAI per WQI.
        let (r0, r1) = (e18(33_798), e18(4_109_058));
        let paired = pair_amount(e18(50), r0, r1);
        let as_f = amount::to_f64(paired, 18);
        assert!((6_070.0..6_090.0).contains(&as_f), "50 WQI pairs with {as_f} WQUAI");
        // An empty reserve pairs with nothing rather than dividing by zero.
        assert_eq!(pair_amount(e18(50), U256::ZERO, r1), U256::ZERO);
    }

    #[test]
    fn minted_liquidity_takes_the_worse_of_the_two_ratios() {
        let (r0, r1, supply) = (e18(1_000), e18(1_000), e18(1_000));
        // A balanced 10% deposit mints 10%.
        assert_eq!(liquidity_minted(e18(100), e18(100), r0, r1, supply), e18(100));
        // Doubling one side alone mints no more: the excess is a gift to the pool.
        assert_eq!(liquidity_minted(e18(200), e18(100), r0, r1, supply), e18(100));
        // An empty pool mints nothing here; the first deposit is handled separately.
        assert_eq!(liquidity_minted(e18(1), e18(1), U256::ZERO, r1, U256::ZERO), U256::ZERO);
    }

    #[test]
    fn redeeming_returns_a_share_of_both_reserves() {
        let (r0, r1, supply) = (e18(1_000), e18(4_000), e18(500));
        let (a, b) = redeemable(e18(50), supply, r0, r1);
        assert_eq!((a, b), (e18(100), e18(400)), "10% of the pool");
        assert_eq!(redeemable(e18(50), U256::ZERO, r0, r1), (U256::ZERO, U256::ZERO));
    }

    #[test]
    fn share_is_reported_honestly_at_the_edges() {
        assert_eq!(share_bps(e18(1), e18(100)), 100);
        assert_eq!(share_bps(e18(1), U256::ZERO), 0, "an empty pool is not an infinite share");
        assert_eq!(share_bps(e18(5), e18(5)), 10_000, "the whole pool");
        let dust = LpPosition { lp_wallet: U256::from(1u64), lp_total: e18(1_000_000), ..LpPosition::default() };
        assert_eq!(dust.share_text(), "<0.01%", "a real holding never reads as 0.00%");
        assert_eq!(LpPosition::default().share_text(), "0.00%", "but nothing held does");
    }

    #[test]
    fn an_empty_pool_warns_the_first_provider() {
        let q = AddLiquidityQuote::build(&pool(), U256::ZERO, U256::ZERO, U256::ZERO, e18(10), Side::Token0, 50);
        assert!(q.first_provider);
        assert_eq!(q.warnings.len(), 1);
        assert!(q.warnings[0].contains("sets its opening price"));
        assert_eq!(q.share_text(), "100% (first provider)");
        // A pool with reserves does not warn.
        let q = AddLiquidityQuote::build(&pool(), e18(1_000), e18(1_000), e18(1_000), e18(10), Side::Token0, 50);
        assert!(!q.first_provider && q.warnings.is_empty());
        assert_eq!(q.amount1, e18(10), "a 1:1 pool pairs one for one");
        assert_eq!(q.amount0_min, minimum(e18(10), 50));
        assert!(q.share_text().starts_with("0.99"), "{}", q.share_text());
    }

    /// Someone rich in one token and short of the other sizes the deposit by the scarce side, so
    /// either side can be the one that is typed.
    #[test]
    fn a_deposit_can_be_sized_from_either_side() {
        // 1 WQI is worth 4 WQUAI here.
        let (r0, r1, supply) = (e18(1_000), e18(4_000), e18(1_000));
        let typed0 = AddLiquidityQuote::build(&pool(), r0, r1, supply, e18(10), Side::Token0, 50);
        assert_eq!((typed0.amount0, typed0.amount1), (e18(10), e18(40)), "10 WQI pulls in 40 WQUAI");
        // Typing the same deposit from the other side asks for the same pair of amounts.
        let typed1 = AddLiquidityQuote::build(&pool(), r0, r1, supply, e18(40), Side::Token1, 50);
        assert_eq!((typed1.amount0, typed1.amount1), (e18(10), e18(40)), "40 WQUAI consumes 10 WQI");
        assert_eq!(typed1.liquidity, typed0.liquidity, "the same deposit mints the same LP");
        assert_eq!(typed1.amount0_min, minimum(e18(10), 50), "both minimums still come from slippage");
        // The side is named by symbol, in either case, and anything else is refused by name.
        assert_eq!(Side::of(&pool(), None).unwrap(), Side::Token0);
        assert_eq!(Side::of(&pool(), Some("wquai")).unwrap(), Side::Token1);
        let err = Side::of(&pool(), Some("SMOL")).unwrap_err().to_string();
        assert!(err.contains("WQI") && err.contains("WQUAI"), "{err}");
    }

    /// An approval that cannot absorb the movement it was made for is stale before it is used:
    /// the derived side is re-priced by every trade against the pool, so its cap carries the same
    /// tolerance the deposit does. The typed side is exact — it does not move.
    #[test]
    fn an_approval_covers_the_side_that_moves() {
        let (r0, r1, supply) = (e18(1_000), e18(4_000), e18(1_000));
        let q = AddLiquidityQuote::build(&pool(), r0, r1, supply, e18(10), Side::Token0, 50);
        assert_eq!(q.approval_cap(false), e18(10), "the typed side is approved exactly");
        assert_eq!(q.approval_cap(true), ceiling(e18(40), 50), "the derived side carries the slippage");
        assert!(q.approval_cap(true) > q.amount1);
        // Typing the other side swaps which one is exact.
        let q = AddLiquidityQuote::build(&pool(), r0, r1, supply, e18(40), Side::Token1, 50);
        assert_eq!(q.approval_cap(true), e18(40));
        assert_eq!(q.approval_cap(false), ceiling(e18(10), 50));
        // Allowances decide what is still to be signed: enough on a side is no step at all.
        let both = q.clone().with_allowances(U256::ZERO, U256::ZERO);
        assert_eq!(both.approvals_needed(), vec![false, true], "neither side approved: both steps");
        let one = q.clone().with_allowances(e18(10), U256::ZERO);
        assert_eq!(one.approvals_needed(), vec![true], "token0 already covers its side");
        let none = q.with_allowances(e18(10), e18(40));
        assert!(none.approvals_needed().is_empty(), "approved for enough: straight to the deposit");
        assert_eq!(none.allowance0.as_deref(), Some(e18(10).to_string().as_str()));
    }

    #[test]
    fn duplicate_gauge_positions_keep_deployment_pid_and_wallet_separate() {
        use crate::gauge::{GaugeKind, GaugePool, GaugeView};
        use crate::zone::{ZonePool, ZoneView};
        let core = GaugeView {
            address: "core".into(),
            pools: vec![GaugePool { pid: 2, lp_token: "pair".into(), ..Default::default() }],
            ..Default::default()
        };
        let zone = ZoneView {
            pools: vec![
                ZonePool { gauge: "zone_a".into(), pid: 5, lp_token: "pair".into(), staked: e18(9), ..Default::default() },
                ZonePool { gauge: "zone_b".into(), pid: 70, lp_token: "pair".into(), staked: e18(7), ..Default::default() },
            ],
            ..Default::default()
        };
        let pool = Pool { address: "pair".into(), ..Default::default() };
        let rows = positions_from_balances(&pool, (e18(100), e18(200)), e18(1), e18(3), e18(100), Some(&core), Some(&zone)).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows.iter().map(|p| p.lp_total_held()).sum::<U256>(), e18(20));
        assert_eq!((rows[0].gauge_address.as_deref(), rows[0].pid, rows[0].gauge), (Some("core"), Some(2), Some(GaugeKind::Core)));
        assert_eq!((rows[1].gauge_address.as_deref(), rows[1].pid, rows[1].lp_staked), (Some("zone_a"), Some(5), e18(9)));
        assert_eq!((rows[2].gauge_address.as_deref(), rows[2].pid, rows[2].lp_staked), (Some("zone_b"), Some(70), e18(7)));
        assert!(rows[1].lp_wallet.is_zero());
        assert!(rows[2].lp_wallet.is_zero());
    }

    #[test]
    fn removing_says_how_much_must_be_unstaked_first() {
        let position = LpPosition {
            pair: "0x00aa".into(),
            token0: token("WQI", 18),
            token1: token("WQUAI", 18),
            lp_wallet: e18(30),
            lp_staked: e18(70),
            lp_total: e18(1_000),
            ..LpPosition::default()
        };
        let (r0, r1) = (e18(2_000), e18(8_000));
        // Half the position is 50 LP; 30 sits in the wallet, so 20 must come out of the gauge.
        let q = RemoveLiquidityQuote::build(&position, r0, r1, 50, 50);
        assert_eq!(q.liquidity, e18(50));
        assert_eq!(q.unstake_first, e18(20));
        assert_eq!((q.amount0, q.amount1), (e18(100), e18(400)));
        assert_eq!(q.amount0_min, minimum(e18(100), 50));
        // Withdrawing only what is already liquid needs no unstake.
        assert!(RemoveLiquidityQuote::build(&position, r0, r1, 30, 50).unstake_first.is_zero());
        // Percent is clamped, so a bad caller cannot burn more than is held.
        assert_eq!(RemoveLiquidityQuote::build(&position, r0, r1, 200, 50).liquidity, e18(100));
    }

    #[test]
    fn impermanent_loss_is_zero_at_parity_and_negative_either_side() {
        assert_eq!(impermanent_loss_pct(1.0), 0.0);
        // The textbook figures: a 2x move costs ~5.7%, a 4x move ~20%.
        assert!((impermanent_loss_pct(2.0) + 5.72).abs() < 0.05, "{}", impermanent_loss_pct(2.0));
        assert!((impermanent_loss_pct(4.0) + 20.0).abs() < 0.1, "{}", impermanent_loss_pct(4.0));
        // Symmetric: halving hurts exactly as much as doubling.
        assert!((impermanent_loss_pct(0.5) - impermanent_loss_pct(2.0)).abs() < 1e-9);
        // Nonsense input is not a loss figure.
        assert_eq!(impermanent_loss_pct(0.0), 0.0);
        assert_eq!(impermanent_loss_pct(f64::NAN), 0.0);
    }

    #[test]
    fn position_text_reads_like_a_holding() {
        let p = LpPosition {
            pair: "0x00aa".into(),
            token0: token("WQI", 18),
            token1: token("WQUAI", 18),
            lp_wallet: e18(2),
            lp_staked: e18(2),
            lp_total: e18(1_000),
            amount0: e18(18),
            amount1: e18(2_214),
            usd: Some(284.10),
            pid: Some(0),
            gauge: Some(crate::gauge::GaugeKind::Core),
            gauge_address: Some("core".into()),
        };
        assert_eq!(p.name(), "WQI/WQUAI");
        assert_eq!(p.share_text(), "0.40%");
        assert_eq!(p.underlying_text(), "18 WQI + 2,214 WQUAI");
        assert!(!p.is_empty());
        assert_eq!(p.lp_total_held(), e18(4));
    }

    #[test]
    fn the_liquidity_abi_parses() {
        quai_sdk::abi::AbiInterface::from_human_readable(LIQUIDITY_ABI).unwrap();
        quai_sdk::abi::AbiInterface::from_human_readable(LP_TOKEN_ABI).unwrap();
    }
}

#[cfg(test)]
mod trading_precision_regressions {
    use super::*;

    #[test]
    fn small_lp_value_and_raw_reserves_survive_display_precision() {
        let pool = Pool { tvl_usd: Some(67_150.0), reserve0: 0.0, reserve1: 0.0, ..Pool::default() };
        let raw = U256::from(10u64).pow(U256::from(24)) + U256::from(12345);
        let position = build_position(&pool, (raw, raw), U256::from(1), U256::ZERO, U256::from(20_000), None, None).unwrap();
        assert!((position.usd.unwrap() - 3.3575).abs() < 1e-10);
        assert_eq!(position.amount0, raw / U256::from(20_000));
        assert_eq!(position.share_text(), "<0.01%");
    }

    #[test]
    fn half_of_one_signer_position_does_not_spend_another_owners_share() {
        let p = LpPosition { lp_wallet: U256::from(100), lp_total: U256::from(1000), ..LpPosition::default() };
        let q = RemoveLiquidityQuote::build(&p, U256::from(1000), U256::from(2000), 50, 50);
        assert_eq!(q.liquidity, U256::from(50));
        assert_eq!((q.amount0, q.amount1), (U256::from(50), U256::from(100)));
        assert!(validate_percent(0).is_err());
        assert!(validate_percent(101).is_err());
    }
}
