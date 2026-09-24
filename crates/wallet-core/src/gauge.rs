//! Quainance's PoolGauge: staking LP for reward streams.
//!
//! The gauge is Synthetix-shaped, not a MasterChef — rewards are per `(pid, reward token)` streams
//! with a rate and an end, and a pool can carry several reward tokens at once.
//!
//! **The deployed gauge is older than the ABI Quainance's frontend ships.** Every signature here
//! was probed against chain 9 on 2026-09-16; the published extras (`stakeWithPermit`,
//! `emergencyWithdraw`, sponsor and genesis campaigns) do not exist on it and are deliberately
//! absent below. There is no emergency exit: `withdraw` is the only way out, and it settles
//! rewards on the way.
//!
//! Staking, unstaking and claiming also serve the launch-zone gauges (`zone.rs`), which share
//! those four signatures: `Session::stake_target` decides which gauge a pair belongs to.

use crate::amount;
use crate::markets::PoolToken;
use quai_sdk::U256;
use serde::{Deserialize, Serialize};

/// The gauge ABI, as deployed.
pub const GAUGE_ABI: &[&str] = &[
    "function poolLength() view returns (uint256)",
    "function poolInfo(uint256 pid) view returns (address lpToken, uint256 totalStaked)",
    "function getPoolId(address pair) view returns (uint256 pid, bool exists)",
    "function balanceOf(uint256 pid, address user) view returns (uint256)",
    "function earned(uint256 pid, address user, address rewardToken) view returns (uint256)",
    "function rewardTokenLength(uint256 pid) view returns (uint256)",
    "function rewardTokens(uint256 pid, uint256 index) view returns (address)",
    "function rewardData(uint256 pid, address rewardToken) view returns (uint256 periodFinish, uint256 rewardRate, uint256 lastUpdateTime, uint256 rewardPerTokenStored)",
    "function factory() view returns (address)",
    "function stake(uint256 pid, uint256 amount)",
    "function withdraw(uint256 pid, uint256 amount)",
    "function getReward(uint256 pid, address[] tokens)",
    "function exit(uint256 pid, address[] tokens)",
    "function notifyRewardAmount(uint256 pid, address rewardToken, uint256 amount, uint256 duration)",
];

/// The two unrelated gauge families a Quainance pair can be staked in.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GaugeKind {
    /// The PoolGauge in this module.
    Core,
    /// A launch-zone gauge (`zone.rs`), carrying genesis campaigns on launched tokens.
    Zone,
}

impl GaugeKind {
    /// What a review calls the contract.
    pub fn label(self) -> &'static str {
        match self {
            GaugeKind::Core => "Quainance gauge",
            GaugeKind::Zone => "launch-zone gauge",
        }
    }
}

/// Reward tokens the gauge accepts, allowlisted at launch (`llms.txt`). Funding anything else
/// reverts, so the wallet refuses it up front rather than letting the user pay for a failure.
pub const ALLOWED_REWARD_SYMBOLS: [&str; 3] = ["WQUAI", "WQI", "USDT"];

/// Extra precision above reward-token atoms. The exact pinned runtime was executed locally
/// with 6-, 8- and 18-decimal mock tokens: `notifyRewardAmount` sets `amount * 1e18 / duration`.
/// See `tests/fixtures/gauge_rate_evidence.json` and `gauge_rate_vm.mjs`; the live immutable
/// `REWARD_RATE_PRECISION()` getter also returns 1e18. Zone gauges use a separate atoms/sec ABI.
pub const REWARD_RATE_SCALE: u32 = 18;

/// One reward stream on a pool.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct RewardStream {
    pub token: PoolToken,
    /// Raw `rewardRate`, scaled by 1e18 above the token's own decimals.
    #[serde(with = "crate::explorer::u256_string")]
    pub rate: U256,
    /// Unix seconds the stream stops paying.
    pub period_finish: u64,
    /// What this account has earned but not claimed.
    #[serde(with = "crate::explorer::u256_string")]
    pub earned: U256,
}

impl RewardStream {
    /// The stream is still paying at `now`.
    pub fn live(&self, now: u64) -> bool {
        self.period_finish > now && !self.rate.is_zero()
    }

    /// Whole reward tokens emitted per day across the whole pool.
    pub fn per_day(&self) -> f64 {
        emission_per_day(self.rate, self.token.decimals)
    }

    /// `ends in 55d`, or `ended`.
    pub fn period_text(&self, now: u64) -> String {
        if !self.live(now) {
            return "ended".into();
        }
        format!("ends in {}", crate::track::human_duration(self.period_finish.saturating_sub(now)))
    }

    /// `12.4 WQUAI`.
    pub fn earned_text(&self) -> String {
        format!("{} {}", amount::format_amount_short(self.earned, self.token.decimals, 4), self.token.symbol)
    }
}

/// Whole reward tokens per day for a raw `rewardRate`.
pub fn emission_per_day(rate: U256, decimals: u8) -> f64 {
    if rate.is_zero() {
        return 0.0;
    }
    // Convert atoms using this token's units, then remove the contract's extra precision.
    let per_second = amount::to_f64(rate, decimals) / 10f64.powi(REWARD_RATE_SCALE as i32);
    per_second * 86_400.0
}

/// One gauge pool: its LP, what is staked, and what it pays.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct GaugePool {
    pub pid: u64,
    /// LP token (the pair contract), lowercase.
    pub lp_token: String,
    /// LP staked by everyone.
    #[serde(with = "crate::explorer::u256_string")]
    pub total_staked: U256,
    /// The pair's whole LP supply, so `total_staked` can be turned into a share of the pool — and
    /// so an APR can be quoted for a pool this wallet holds nothing in.
    #[serde(with = "crate::explorer::u256_string")]
    #[serde(default)]
    pub lp_supply: U256,
    /// LP staked by this account.
    #[serde(with = "crate::explorer::u256_string")]
    pub staked: U256,
    pub rewards: Vec<RewardStream>,
}

/// APR coverage travels with its value; a partial reward basket is a lower bound.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct AprEstimate {
    pub percent: f64,
    pub partial: bool,
}

impl AprEstimate {
    pub fn text(self) -> String {
        format!("{}{:.1}%", if self.partial { "≥ " } else { "" }, self.percent)
    }
}

impl GaugePool {
    /// Any live stream.
    pub fn active(&self, now: u64) -> bool {
        self.rewards.iter().any(|r| r.live(now))
    }

    /// Anything claimable right now.
    pub fn has_rewards(&self) -> bool {
        self.rewards.iter().any(|r| !r.earned.is_zero())
    }

    /// Reward tokens, for `getReward` and `exit`.
    pub fn reward_addresses(&self) -> Vec<String> {
        self.rewards.iter().map(|r| r.token.address.clone()).collect()
    }

    /// Annualised return on the value staked, in percent.
    ///
    /// `reward_usd` prices one whole reward token; `staked_usd` is the USD value of all LP in the
    /// gauge for this pool. Returns None when either is unknown or nothing is staked — an APR the
    /// wallet cannot derive is shown as `—`, never as `0%`.
    ///
    /// The figure moves as others stake, so it is computed fresh and never cached into a review.
    pub fn apr(&self, now: u64, reward_usd: &dyn Fn(&PoolToken) -> Option<f64>, staked_usd: Option<f64>) -> Option<f64> {
        self.apr_estimate(now, reward_usd, staked_usd).filter(|v| !v.partial).map(|v| v.percent)
    }

    pub fn apr_estimate(&self, now: u64, reward_usd: &dyn Fn(&PoolToken) -> Option<f64>, staked_usd: Option<f64>) -> Option<AprEstimate> {
        let staked_usd = staked_usd.filter(|v| *v > 0.0)?;
        let mut yearly = 0.0;
        let mut priced = false;
        let mut partial = false;
        for stream in self.rewards.iter().filter(|r| r.live(now)) {
            let Some(price) = reward_usd(&stream.token).filter(|v| v.is_finite() && *v >= 0.0) else {
                partial = true;
                continue;
            };
            yearly += stream.per_day() * 365.0 * price;
            priced = true;
        }
        priced.then(|| yearly / staked_usd * 100.0).filter(|v| v.is_finite()).map(|percent| AprEstimate { percent, partial })
    }
}

impl GaugePool {
    /// The share of the pair's LP that is staked here, in basis points.
    pub fn staked_share_bps(&self) -> u64 {
        crate::liquidity::share_bps(self.total_staked, self.lp_supply)
    }

    /// APR from the pool's own TVL, for a pool this wallet may hold nothing in.
    pub fn apr_estimate_from_tvl(
        &self,
        now: u64,
        reward_usd: &dyn Fn(&PoolToken) -> Option<f64>,
        pool_tvl_usd: Option<f64>,
    ) -> Option<AprEstimate> {
        let staked_usd = pool_tvl_usd.zip(amount::ratio(self.total_staked, self.lp_supply)).map(|(tvl, share)| tvl * share);
        self.apr_estimate(now, reward_usd, staked_usd)
    }

    pub fn apr_from_tvl(&self, now: u64, reward_usd: &dyn Fn(&PoolToken) -> Option<f64>, pool_tvl_usd: Option<f64>) -> Option<f64> {
        let staked_usd = pool_tvl_usd.zip(amount::ratio(self.total_staked, self.lp_supply)).map(|(tvl, share)| tvl * share);
        self.apr(now, reward_usd, staked_usd)
    }
}

/// `23.9%`, or `—` when it cannot be derived.
pub fn apr_text(apr: Option<f64>) -> String {
    apr.map_or_else(|| "—".into(), |v| format!("{v:.1}%"))
}

// ------------------------------------------------------------------------ reading the gauge

use crate::chain::{addr, address_at, uint};
use crate::data::{DataCtx, READ_CALLER};
use crate::error::{CoreError, Result};
use quai_sdk::abi::AbiInterface;
use quai_sdk::contracts::Contract;
use quai_sdk::{BlockTag, QuaiAddress};
use serde_json::{Value, json};

fn interface() -> Result<AbiInterface> {
    crate::chain::interface(GAUGE_ABI)
}

/// The gauge's pools, read once and reused by every position.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct GaugeView {
    /// Gauge address (lowercase).
    pub address: String,
    pub pools: Vec<GaugePool>,
    /// Pools the ceiling kept out, when the gauge has more than [`MAX_GAUGE_POOLS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted: Option<crate::markets::Omitted>,
}

/// Pools one gauge read covers, newest pid first.
///
/// Every pool is a handful of batched reads, so this is generous rather than tight; it exists so
/// that an unbounded `poolLength()` cannot turn one screen into thousands of calls. When it does
/// apply, the view says so instead of quietly showing a shorter list.
pub const MAX_GAUGE_POOLS: u64 = 256;

/// Without a Multicall3 each field is its own call, so the ceiling is much lower.
pub const MAX_GAUGE_POOLS_SEQUENTIAL: u64 = 64;

/// Reward streams read on one pool. Past this the `earned` figure would under-report, so it is
/// set well above what any Quainance pool carries rather than tight. The claim itself is not
/// affected: `getReward` pays every stream on-chain, whatever the wallet lists.
pub const MAX_REWARD_STREAMS: u64 = 16;

impl GaugeView {
    /// The gauge pool id for a pair, when it has one. Most pairs do not.
    pub fn pid_for(&self, pair: &str) -> Option<u64> {
        self.pools.iter().find(|p| p.lp_token.eq_ignore_ascii_case(pair)).map(|p| p.pid)
    }

    /// The pool behind a pair.
    pub fn pool_for(&self, pair: &str) -> Option<&GaugePool> {
        self.pools.iter().find(|p| p.lp_token.eq_ignore_ascii_case(pair))
    }

    /// LP these owners have staked in a pool.
    pub async fn staked_by(&self, ctx: &DataCtx, pid: u64, owners: &[String]) -> Result<U256> {
        let gauge = Contract::new(addr(&self.address)?, interface()?, &ctx.node.provider);
        let caller = addr(READ_CALLER)?;
        let mut total = U256::ZERO;
        for owner in owners {
            let out = gauge.call(caller, "balanceOf", &[json!(pid.to_string()), json!(owner)], BlockTag::Latest).await?;
            total = total.saturating_add(uint(&out, 0));
        }
        Ok(total)
    }
}

/// Read the gauge: its pools, their reward streams, and what these owners have staked and earned.
///
/// The pin is verified before anything is read, and the gauge's own `factory()` is checked against
/// the factory this network pins — the same two-sided check `swap::Router::open` makes.
pub async fn open(ctx: &DataCtx, owners: &[String]) -> Result<GaugeView> {
    open_selected(ctx, owners, None).await
}

/// A known pair is resolved directly, independent of the browsing directory's pool ceiling.
pub(crate) async fn for_pair(ctx: &DataCtx, owners: &[String], pair: &str) -> Result<GaugeView> {
    open_selected(ctx, owners, Some(pair)).await
}

async fn open_selected(ctx: &DataCtx, owners: &[String], pair: Option<&str>) -> Result<GaugeView> {
    let eco = &ctx.network.ecosystem;
    let pin = eco.quainance_gauge.as_ref().ok_or_else(|| CoreError::NotFound(format!("no gauge on {}", ctx.network.name)))?;
    let gauge_address = ctx.verify_pinned(pin, "Quainance gauge").await?;
    let gauge = Contract::new(gauge_address, interface()?, &ctx.node.provider);
    let caller = addr(READ_CALLER)?;
    if let Some(factory) = eco.quainance_factory.as_ref() {
        let seen = address_at(&gauge.call(caller, "factory", &[], BlockTag::Latest).await?, 0);
        if !seen.eq_ignore_ascii_case(&factory.address) {
            return Err(CoreError::Rejected("the gauge's factory does not match the pinned factory".into()));
        }
    }
    let count = u64::try_from(uint(&gauge.call(caller, "poolLength", &[], BlockTag::Latest).await?, 0)).unwrap_or(0);
    let batched = crate::multicall::Multicall::open(ctx).await.is_some();
    let cap = if batched { MAX_GAUGE_POOLS } else { MAX_GAUGE_POOLS_SEQUENTIAL };
    // Newest pid first: `poolLength` grows by appending, so a ceiling that keeps the oldest would
    // hide exactly the pools a new campaign just added.
    let pids: Vec<u64> = if let Some(pair) = pair {
        let answer = gauge.call(caller, "getPoolId", &[json!(pair)], BlockTag::Latest).await?;
        if answer.get(1).and_then(Value::as_bool) != Some(true) {
            return Err(CoreError::NotFound(format!("pool {pair} is not enrolled in this gauge")));
        }
        vec![u64::try_from(uint(&answer, 0)).map_err(|_| CoreError::Invalid("gauge pool id exceeds supported range".into()))?]
    } else {
        (0..count).rev().take(usize::try_from(cap).unwrap_or(usize::MAX)).collect()
    };
    let omitted = (count > cap).then(|| crate::markets::Omitted {
        source: "Quainance gauge".into(),
        read: pids.len(),
        total: usize::try_from(count).unwrap_or(usize::MAX),
    });
    let address = gauge_address.to_string().to_lowercase();
    if let Some(pools) = batched_pools(ctx, &address, &pids, owners).await {
        verify_selected_pair(pair, &pools)?;
        return Ok(GaugeView { address, pools, omitted });
    }
    // No Multicall3: read the gauge level by level. Correct, just chattier.
    let mut pools = Vec::new();
    for pid in pids.iter().copied() {
        let info = gauge.call(caller, "poolInfo", &[json!(pid.to_string())], BlockTag::Latest).await?;
        let lp_token = address_at(&info, 0);
        if lp_token.is_empty() {
            continue;
        }
        let mut staked = U256::ZERO;
        for owner in owners {
            let out = gauge.call(caller, "balanceOf", &[json!(pid.to_string()), json!(owner)], BlockTag::Latest).await?;
            staked = staked.saturating_add(uint(&out, 0));
        }
        let reward_count =
            u64::try_from(uint(&gauge.call(caller, "rewardTokenLength", &[json!(pid.to_string())], BlockTag::Latest).await?, 0))
                .unwrap_or(0);
        let mut rewards = Vec::new();
        if reward_count > MAX_REWARD_STREAMS {
            return Err(CoreError::Invalid("gauge reward list exceeds the supported bound; principal withdrawal remains available".into()));
        }
        for index in 0..reward_count {
            let out = gauge.call(caller, "rewardTokens", &[json!(pid.to_string()), json!(index.to_string())], BlockTag::Latest).await?;
            let token_address = address_at(&out, 0);
            if token_address.is_empty() {
                continue;
            }
            let token = crate::markets::token_meta_required(ctx, &token_address).await?;
            let data = gauge.call(caller, "rewardData", &[json!(pid.to_string()), json!(token_address)], BlockTag::Latest).await?;
            let mut earned = U256::ZERO;
            for owner in owners {
                let out =
                    gauge.call(caller, "earned", &[json!(pid.to_string()), json!(owner), json!(token.address)], BlockTag::Latest).await?;
                earned = earned.saturating_add(uint(&out, 0));
            }
            rewards.push(RewardStream { token, rate: uint(&data, 1), period_finish: u64::try_from(uint(&data, 0)).unwrap_or(0), earned });
        }
        let lp_supply = Contract::new(
            addr(&lp_token)?,
            AbiInterface::from_human_readable(&["function totalSupply() view returns (uint256)"])
                .map_err(|e| CoreError::Invalid(format!("lp abi: {e}")))?,
            &ctx.node.provider,
        )
        .call(caller, "totalSupply", &[], BlockTag::Latest)
        .await
        .map(|v| uint(&v, 0))?;
        pools.push(GaugePool { pid, lp_token, total_staked: uint(&info, 1), lp_supply, staked, rewards });
    }
    verify_selected_pair(pair, &pools)?;
    Ok(GaugeView { address: gauge_address.to_string().to_lowercase(), pools, omitted })
}

fn verify_selected_pair(pair: Option<&str>, pools: &[GaugePool]) -> Result<()> {
    if let Some(pair) = pair
        && (pools.len() != 1 || !pools[0].lp_token.eq_ignore_ascii_case(pair))
    {
        return Err(CoreError::Rejected("gauge pool id does not match the requested LP token".into()));
    }
    Ok(())
}

/// The whole gauge in three batches instead of one call per field.
///
/// The reads form a dependency chain — pool info, then how many reward tokens, then which, then
/// their rates — so each *level* is batched rather than the whole thing at once. Three round trips
/// for three pools with one account, against roughly twenty sequentially.
async fn batched_pools(ctx: &DataCtx, gauge: &str, pids: &[u64], owners: &[String]) -> Option<Vec<GaugePool>> {
    use crate::multicall::{Arg, Call, Multicall, address_word, word};
    let mc = Multicall::open(ctx).await?;
    let pid_arg = |pid: u64| Arg::Uint(U256::from(pid));
    // Level 1: each pool's LP and total, its reward-token count, and what these owners staked.
    let mut calls = Vec::new();
    for &pid in pids {
        calls.push(Call::view(gauge, "poolInfo(uint256)", &[pid_arg(pid)]));
        calls.push(Call::view(gauge, "rewardTokenLength(uint256)", &[pid_arg(pid)]));
        for owner in owners {
            calls.push(Call::view(gauge, "balanceOf(uint256,address)", &[pid_arg(pid), Arg::Addr(owner.clone())]));
        }
    }
    let level1 = mc.try_all(&calls).await.ok()?;
    if level1.len() != calls.len() || level1.iter().any(|row| row.as_ref().is_none_or(|data| data.len() < 32)) {
        return None;
    }
    let mut drafts: Vec<(u64, String, U256, U256, u64)> = Vec::new();
    let mut i = 0;
    for &pid in pids {
        let info = level1.get(i).and_then(Option::as_ref).filter(|d| d.len() >= 64)?;
        i += 1;
        let reward_count = level1.get(i).and_then(Option::as_ref).map_or(U256::ZERO, |d| word(d, 0));
        i += 1;
        let mut staked = U256::ZERO;
        for _ in owners {
            staked = staked.saturating_add(level1.get(i).and_then(Option::as_ref).map_or(U256::ZERO, |d| word(d, 0)));
            i += 1;
        }
        let lp_token = address_word(info, 0);
        if lp_token.is_empty() || lp_token.trim_start_matches("0x").chars().all(|c| c == '0') {
            continue;
        }
        let reward_count = u64::try_from(reward_count).ok()?;
        if reward_count > MAX_REWARD_STREAMS {
            return None;
        }
        drafts.push((pid, lp_token, word(info, 1), staked, reward_count));
    }
    // Level 2: which reward tokens each pool carries.
    let mut calls = Vec::new();
    for (pid, lp_token, _, _, n) in &drafts {
        // The pair's LP supply rides along here: it is only knowable once poolInfo named the pair.
        calls.push(Call::view(lp_token, "totalSupply()", &[]));
        for index in 0..*n {
            calls.push(Call::view(gauge, "rewardTokens(uint256,uint256)", &[pid_arg(*pid), Arg::Uint(U256::from(index))]));
        }
    }
    let level2 = mc.try_all(&calls).await.ok()?;
    if level2.len() != calls.len() || level2.iter().any(|row| row.as_ref().is_none_or(|data| data.len() < 32)) {
        return None;
    }
    let mut tokens: Vec<Vec<String>> = Vec::new();
    let mut supplies: Vec<U256> = Vec::new();
    let mut i = 0;
    for (_, _, _, _, n) in &drafts {
        supplies.push(level2.get(i).and_then(Option::as_ref).map_or(U256::ZERO, |d| word(d, 0)));
        i += 1;
        let mut list = Vec::new();
        for _ in 0..*n {
            if let Some(d) = level2.get(i).and_then(Option::as_ref) {
                let a = address_word(d, 0);
                if !a.is_empty() && !a.trim_start_matches("0x").chars().all(|c| c == '0') {
                    list.push(a);
                }
            }
            i += 1;
        }
        tokens.push(list);
    }
    // Level 3: each stream's rate and end, and what these owners have earned on it.
    let mut calls = Vec::new();
    for ((pid, ..), list) in drafts.iter().zip(&tokens) {
        for token in list {
            calls.push(Call::view(gauge, "rewardData(uint256,address)", &[pid_arg(*pid), Arg::Addr(token.clone())]));
            for owner in owners {
                calls.push(Call::view(
                    gauge,
                    "earned(uint256,address,address)",
                    &[pid_arg(*pid), Arg::Addr(owner.clone()), Arg::Addr(token.clone())],
                ));
            }
        }
    }
    let level3 = mc.try_all(&calls).await.ok()?;
    if level3.len() != calls.len() || level3.iter().any(|row| row.as_ref().is_none_or(|data| data.len() < 32)) {
        return None;
    }
    let mut pools = Vec::new();
    let mut i = 0;
    for (((pid, lp_token, total_staked, staked, _), list), lp_supply) in drafts.iter().zip(&tokens).zip(&supplies) {
        let mut rewards = Vec::new();
        for token in list {
            let data = level3.get(i).and_then(Option::as_ref).filter(|d| d.len() >= 128)?.clone();
            i += 1;
            let mut earned = U256::ZERO;
            for _ in owners {
                earned = earned.saturating_add(level3.get(i).and_then(Option::as_ref).map_or(U256::ZERO, |d| word(d, 0)));
                i += 1;
            }
            rewards.push(RewardStream {
                token: crate::markets::token_meta_required(ctx, token).await.ok()?,
                rate: word(&data, 1),
                period_finish: u64::try_from(word(&data, 0)).unwrap_or(0),
                earned,
            });
        }
        pools.push(GaugePool {
            pid: *pid,
            lp_token: lp_token.clone(),
            total_staked: *total_staked,
            lp_supply: *lp_supply,
            staked: *staked,
            rewards,
        });
    }
    Some(pools)
}

// ------------------------------------------------------------------------ staking actions

use crate::session::Session;
use crate::tx::{AccountRequest, Review, field};

/// How a gauge action is priced in gas. Staking touches every reward stream's accounting, so it
/// scales with how many the pool carries.
fn gauge_gas(base: u64, rewards: usize) -> u64 {
    base + 60_000 * rewards as u64
}

/// One reward stream as a staking review needs it, whichever gauge carries it.
#[derive(Clone, Debug)]
pub struct TargetReward {
    pub token: PoolToken,
    pub earned: U256,
    /// `ends in 55d`, or `ended`.
    pub period: String,
}

impl TargetReward {
    fn earned_text(&self) -> String {
        format!("{} {}", amount::format_amount_short(self.earned, self.token.decimals, 4), self.token.symbol)
    }
}

/// The gauge a pair stakes into, read fresh for a review.
#[derive(Clone, Debug)]
pub struct StakeTarget {
    pub kind: GaugeKind,
    pub address: QuaiAddress,
    pub pid: u64,
    /// LP staked here by this wallet's accounts.
    pub staked: U256,
    pub rewards: Vec<TargetReward>,
    /// The pin's trust label, for the review's spender line.
    pub trust: &'static str,
    /// Something the user should know before staking that this gauge's state implies.
    pub stake_warning: Option<String>,
}

impl StakeTarget {
    fn contract<'a>(&self, provider: &'a crate::network::WalletProvider) -> Result<Contract<'a, crate::network::WalletTransport>> {
        let abi = match self.kind {
            GaugeKind::Core => interface()?,
            GaugeKind::Zone => crate::zone::interface()?,
        };
        Ok(Contract::new(self.address, abi, provider))
    }

    fn has_rewards(&self) -> bool {
        self.rewards.iter().any(|r| !r.earned.is_zero())
    }

    /// `SMOL/WQI (launch-zone gauge, pid 0)`.
    fn pool_text(&self, pair: &str) -> String {
        format!("{pair} ({}, pid {})", self.kind.label(), self.pid)
    }
}

fn select_target(mut targets: Vec<StakeTarget>, pair: &str) -> Result<StakeTarget> {
    match targets.len() {
        0 => Err(CoreError::NotFound(format!("pool {pair} is not enrolled in the selected gauge"))),
        1 => Ok(targets.remove(0)),
        _ => Err(CoreError::Invalid(format!(
            "pool {pair} is enrolled in multiple gauges; select a gauge explicitly: {}",
            targets.iter().map(|t| format!("{} (pid {})", t.address, t.pid)).collect::<Vec<_>>().join(", ")
        ))),
    }
}

/// Principal-only reads do not depend on reward metadata, prices or capped directory scans.
pub(crate) async fn staked_for_pair_at(ctx: &DataCtx, owner: &str, pair: &str, block: BlockTag) -> Result<U256> {
    principal_targets_at(ctx, owner, pair, None, block).await?.iter().try_fold(U256::ZERO, |sum, target| {
        sum.checked_add(target.staked).ok_or_else(|| CoreError::Invalid("combined stake exceeds token range".into()))
    })
}

async fn principal_targets(ctx: &DataCtx, owner: &str, pair: &str, selected: Option<&str>) -> Result<Vec<StakeTarget>> {
    principal_targets_at(ctx, owner, pair, selected, BlockTag::Latest).await
}

async fn principal_targets_at(ctx: &DataCtx, owner: &str, pair: &str, selected: Option<&str>, block: BlockTag) -> Result<Vec<StakeTarget>> {
    let mut targets = Vec::new();
    let core = ctx.network.ecosystem.quainance_gauge.iter().map(|p| (p, false));
    let zones = ctx.network.ecosystem.zone_gauges.iter().map(|p| (p, true));
    for (pin, zone) in core.chain(zones) {
        if selected.is_some_and(|s| !s.eq_ignore_ascii_case(&pin.address)) {
            continue;
        }
        let address = ctx.verify_pinned(pin, "staking gauge").await?;
        let abi = if zone { crate::zone::interface()? } else { interface()? };
        let gauge = Contract::new(address, abi, &ctx.node.provider);
        let caller = addr(READ_CALLER)?;
        let answer = gauge.call(caller, "getPoolId", &[json!(pair)], block).await?;
        if answer.get(1).and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let pid = u64::try_from(uint(&answer, 0)).map_err(|_| CoreError::Invalid("gauge pool id exceeds supported range".into()))?;
        let info = gauge.call(caller, if zone { "poolSummary" } else { "poolInfo" }, &[json!(pid.to_string())], block).await?;
        if !address_at(&info, 0).eq_ignore_ascii_case(pair) {
            return Err(CoreError::Rejected("gauge pool id does not match the requested LP token".into()));
        }
        let balance = gauge.call(caller, "balanceOf", &[json!(pid.to_string()), json!(owner)], block).await?;
        targets.push(StakeTarget {
            kind: if zone { GaugeKind::Zone } else { GaugeKind::Core },
            address,
            pid,
            staked: uint(&balance, 0),
            rewards: Vec::new(),
            trust: pin.trust_label_on(&ctx.node),
            stake_warning: None,
        });
    }
    Ok(targets)
}

impl Session {
    /// The core gauge for this network, with the pool behind a pair. Funding rewards goes here only.
    ///
    /// Only reviews call this, so the gauge's pin and its figures are read first-hand.
    async fn gauge_pool(&mut self, pair: &str) -> Result<(QuaiAddress, GaugePool)> {
        let ctx = self.data_ctx_at(crate::data::Trust::FirstHand)?;
        let view = for_pair(&ctx, &[], pair).await?;
        let pool = view.pool_for(pair).cloned().ok_or_else(|| {
            CoreError::NotFound(format!(
                "pool {pair} is not in the Quainance gauge — only its pools can be funded; launch-zone campaigns are funded when a token launches"
            ))
        })?;
        Ok((addr(&view.address)?, pool))
    }

    /// The gauge a pair stakes into: the core gauge when it lists the pair, otherwise whichever
    /// pinned launch-zone gauge does. Positions resolve a pair in the same order.
    pub async fn stake_target(&mut self, pair: &str) -> Result<StakeTarget> {
        self.stake_target_for(None, pair).await
    }

    pub async fn stake_target_for(&mut self, account: Option<&str>, pair: &str) -> Result<StakeTarget> {
        self.stake_target_in_gauge(account, pair, None).await
    }

    /// Explicit gauge identity resolves duplicate enrollment without selecting another holding.
    pub async fn stake_target_in_gauge(&mut self, account: Option<&str>, pair: &str, selected: Option<&str>) -> Result<StakeTarget> {
        let ctx = self.data_ctx_at(crate::data::Trust::FirstHand)?;
        let owners = vec![self.account(account)?.address];
        let now = crate::registry::now();
        let mut targets = Vec::new();
        if self.network.ecosystem.quainance_gauge.as_ref().is_some_and(|p| selected.is_none_or(|s| s.eq_ignore_ascii_case(&p.address))) {
            match for_pair(&ctx, &owners, pair).await {
                Ok(view) => {
                    for pool in &view.pools {
                        targets.push(StakeTarget {
                            kind: GaugeKind::Core,
                            address: addr(&view.address)?,
                            pid: pool.pid,
                            staked: pool.staked,
                            rewards: pool
                                .rewards
                                .iter()
                                .map(|r| TargetReward { token: r.token.clone(), earned: r.earned, period: r.period_text(now) })
                                .collect(),
                            trust: self.network.ecosystem.quainance_gauge.as_ref().map_or("", |p| p.trust_label_on(&self.node)),
                            stake_warning: None,
                        });
                    }
                }
                Err(CoreError::NotFound(_)) => {}
                Err(error) => return Err(error),
            }
        }
        let zone = crate::zone::for_pair_in_gauge(&ctx, &owners, pair, selected).await?;
        for pool in &zone.pools {
            let trust = self
                .network
                .ecosystem
                .zone_gauges
                .iter()
                .find(|p| p.address.eq_ignore_ascii_case(&pool.gauge))
                .map_or("", |p| p.trust_label_on(&self.node));
            let stake_warning = if pool.active(now) {
                None
            } else if pool.campaign.state(now) == crate::zone::Genesis::AwaitingActivation {
                Some(format!(
                    "this pool's campaign has not started: it pays once {} LP is staked in total ({:.0}% of the way there)",
                    amount::format_amount_short(pool.activation_threshold, 18, 2),
                    pool.activation_bps() as f64 / 100.0
                ))
            } else {
                Some("nothing is paying on this pool right now — staked LP earns no rewards until a stream is funded".into())
            };
            targets.push(StakeTarget {
                kind: GaugeKind::Zone,
                address: addr(&pool.gauge)?,
                pid: pool.pid,
                staked: pool.staked,
                rewards: pool
                    .rewards
                    .iter()
                    .map(|r| TargetReward {
                        token: r.token.clone(),
                        earned: r.earned,
                        period: if r.live(now) {
                            format!("ends in {}", crate::track::human_duration(r.period_finish.saturating_sub(now)))
                        } else {
                            "ended".into()
                        },
                    })
                    .collect(),
                trust,
                stake_warning,
            });
        }
        select_target(targets, pair)
    }

    /// Review the exact LP approval staking needs (step 1 of 2).
    pub async fn review_stake_approval(&mut self, account: Option<&str>, pair: &str, value: &str, max_fee: Option<&str>) -> Result<Review> {
        self.review_stake_approval_in_gauge(account, pair, None, value, max_fee).await
    }

    pub async fn review_stake_approval_in_gauge(
        &mut self,
        account: Option<&str>,
        pair: &str,
        selected: Option<&str>,
        value: &str,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        let atoms = crate::amount::parse_amount(value, 18)?;
        crate::swap::require_minimum(atoms)?;
        let from = self.account(account)?;
        let target = self.stake_target_in_gauge(account, pair, selected).await?;
        let gauge_address = target.address;
        let erc = quai_sdk::contracts::Erc20::new(addr(pair)?, &self.node.provider)?;
        let balance = erc.balance_of(addr(&from.address)?, addr(&from.address)?, BlockTag::Latest).await?;
        if balance < atoms {
            return Err(CoreError::Insufficient(format!("LP balance is {}", crate::amount::format_amount(balance, 18))));
        }
        let atoms = self.bounded_allowance(pair, addr(&from.address)?, gauge_address, atoms).await?;
        let call = erc.approve(gauge_address, atoms)?;
        let call = crate::data::with_access_list(&self.node.provider, addr(&from.address)?, call).await?;
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: "approve".into(),
            title: "Approve LP for staking (step 1 of 2)".into(),
            asset: "LP".into(),
            amount: atoms,
            decimals: 18,
            counterparty: gauge_address.to_string(),
            fields: vec![
                field("LP token", pair.to_string()),
                field("Spender", format!("{gauge_address} ({}, {})", target.kind.label(), target.trust)),
                field("Allowance", format!("exactly {} LP", crate::amount::format_amount(atoms, 18))),
            ],
            warnings: vec![],
            detail: serde_json::json!({"token": pair, "purpose": "stake", "spender": gauge_address.to_string()}),
            max_gas: 120_000,
            max_fee: self.parse_fee_cap(max_fee, crate::amount::QUAI_DECIMALS)?,
        })
        .await
    }

    /// Review staking LP into the gauge.
    pub async fn review_stake(&mut self, account: Option<&str>, pair: &str, value: &str, max_fee: Option<&str>) -> Result<Review> {
        self.review_stake_in_gauge(account, pair, None, value, max_fee).await
    }

    pub async fn review_stake_in_gauge(
        &mut self,
        account: Option<&str>,
        pair: &str,
        selected: Option<&str>,
        value: &str,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        let atoms = crate::amount::parse_amount(value, 18)?;
        crate::swap::require_minimum(atoms)?;
        let from = self.account(account)?;
        let owner = addr(&from.address)?;
        let target = self.stake_target_in_gauge(account, pair, selected).await?;
        let gauge_address = target.address;
        let erc = quai_sdk::contracts::Erc20::new(addr(pair)?, &self.node.provider)?;
        let balance = erc.balance_of(owner, owner, BlockTag::Latest).await?;
        if balance < atoms {
            return Err(CoreError::Insufficient(format!("LP balance is {}", crate::amount::format_amount(balance, 18))));
        }
        let allowance = erc.allowance(owner, owner, gauge_address, BlockTag::Latest).await?;
        if allowance < atoms {
            return Err(crate::error::approval_needed(pair, "approve the LP for the gauge first (step 1 of 2)"));
        }
        let contract = target.contract(&self.node.provider)?;
        let call = contract.prepare("stake", &[json!(target.pid.to_string()), json!(atoms.to_string())], U256::ZERO)?;
        let call = crate::data::with_access_list(&self.node.provider, owner, call).await?;
        let mut fields =
            vec![field("Pool", target.pool_text(pair)), field("Stake", format!("{} LP", crate::amount::format_amount(atoms, 18)))];
        for reward in &target.rewards {
            fields.push(field("Reward", format!("{} · {}", reward.token.symbol, reward.period)));
        }
        // Users of other farms look for a panic hatch; neither gauge family has one, so say so.
        let mut warnings =
            vec!["this gauge has no emergency withdraw — `withdraw` is the only exit, and it settles rewards on the way".into()];
        warnings.extend(target.stake_warning.clone());
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: "stake".into(),
            title: format!("Stake LP in the {}", target.kind.label()),
            asset: "LP".into(),
            amount: atoms,
            decimals: 18,
            counterparty: gauge_address.to_string(),
            fields,
            warnings,
            detail: serde_json::json!({"pair": pair, "pid": target.pid, "gauge": gauge_address.to_string(), "kind": target.kind,
                "financial_effects":[{"direction":"out","asset":"LP","token":pair,"decimals":18,"amount":atoms.to_string(),"note":"principal held by the gauge until withdrawal"}]}),
            max_gas: gauge_gas(260_000, target.rewards.len()),
            max_fee: self.parse_fee_cap(max_fee, crate::amount::QUAI_DECIMALS)?,
        })
        .await
    }

    /// Review taking LP back out of the gauge.
    pub async fn review_unstake(&mut self, account: Option<&str>, pair: &str, value: &str, max_fee: Option<&str>) -> Result<Review> {
        self.review_unstake_in_gauge(account, pair, None, value, max_fee).await
    }

    pub async fn review_unstake_in_gauge(
        &mut self,
        account: Option<&str>,
        pair: &str,
        selected: Option<&str>,
        value: &str,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        let atoms = crate::amount::parse_amount(value, 18)?;
        crate::swap::require_minimum(atoms)?;
        let from = self.account(account)?;
        let owner = addr(&from.address)?;
        let ctx = self.data_ctx_at(crate::data::Trust::FirstHand)?;
        let target = select_target(principal_targets(&ctx, &from.address, pair, selected).await?, pair)?;
        let gauge_address = target.address;
        if target.staked < atoms {
            return Err(CoreError::Insufficient(format!("staked LP is {}", crate::amount::format_amount(target.staked, 18))));
        }
        let contract = target.contract(&self.node.provider)?;
        let call = contract.prepare("withdraw", &[json!(target.pid.to_string()), json!(atoms.to_string())], U256::ZERO)?;
        let call = crate::data::with_access_list(&self.node.provider, owner, call).await?;
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: "unstake".into(),
            title: format!("Unstake LP from the {}", target.kind.label()),
            asset: "LP".into(),
            amount: atoms,
            decimals: 18,
            counterparty: gauge_address.to_string(),
            fields: vec![
                field("Pool", target.pool_text(pair)),
                field("Unstake", format!("{} LP", crate::amount::format_amount(atoms, 18))),
                field("Still staked", format!("{} LP", crate::amount::format_amount(target.staked.saturating_sub(atoms), 18))),
            ],
            warnings: vec![],
            detail: serde_json::json!({"pair": pair, "pid": target.pid, "gauge": gauge_address.to_string(), "kind": target.kind,
                "financial_effects": [{"direction":"in", "asset":"LP", "token":pair, "decimals":18, "amount":atoms.to_string(), "estimated":false, "note":"unstaked principal"}]}),
            max_gas: gauge_gas(260_000, MAX_REWARD_STREAMS as usize),
            max_fee: self.parse_fee_cap(max_fee, crate::amount::QUAI_DECIMALS)?,
        })
        .await
    }

    /// The reward token a pool can be funded with, resolved from a symbol or an address.
    async fn reward_asset(&mut self, pool: &GaugePool, token: &str) -> Result<PoolToken> {
        let t = token.trim();
        // An existing stream is the surest answer: it is already accepted by this pool.
        if let Some(stream) =
            pool.rewards.iter().find(|r| r.token.symbol.eq_ignore_ascii_case(t) || r.token.address.eq_ignore_ascii_case(t))
        {
            return Ok(stream.token.clone());
        }
        let asset = self.swap_asset(t).await?;
        let crate::swap::SwapAsset::Token { address, symbol, decimals } = asset else {
            return Err(CoreError::Invalid("rewards are paid in tokens; wrap QUAI into WQUAI first".into()));
        };
        let allowed = self.network.wquai.as_ref().is_some_and(|a| a.eq_ignore_ascii_case(&address))
            || self.network.wqi.as_ref().is_some_and(|a| a.eq_ignore_ascii_case(&address))
            || self.network.ecosystem.usdt.as_ref().is_some_and(|p| p.address.eq_ignore_ascii_case(&address));
        if !allowed {
            return Err(CoreError::Rejected(format!(
                "{symbol} is not an allowlisted reward token — the gauge accepts {}",
                ALLOWED_REWARD_SYMBOLS.join(", ")
            )));
        }
        Ok(PoolToken { address, symbol, decimals })
    }

    /// Review the exact approval funding a pool's rewards needs (step 1 of 2).
    pub async fn review_incentivize_approval(
        &mut self,
        account: Option<&str>,
        pair: &str,
        token: &str,
        value: &str,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        let (gauge_address, pool) = self.gauge_pool(pair).await?;
        let reward = self.reward_asset(&pool, token).await?;
        let atoms = crate::amount::parse_amount(value, reward.decimals)?;
        let from = self.account(account)?;
        let owner = addr(&from.address)?;
        let erc = quai_sdk::contracts::Erc20::new(addr(&reward.address)?, &self.node.provider)?;
        let balance = erc.balance_of(owner, owner, BlockTag::Latest).await?;
        if balance < atoms {
            return Err(CoreError::Insufficient(format!(
                "{} balance is {}",
                reward.symbol,
                crate::amount::format_amount(balance, reward.decimals)
            )));
        }
        let atoms = self.bounded_allowance(&reward.address, owner, gauge_address, atoms).await?;
        let call = erc.approve(gauge_address, atoms)?;
        let call = crate::data::with_access_list(&self.node.provider, owner, call).await?;
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: "approve".into(),
            title: format!("Approve {} to fund rewards (step 1 of 2)", reward.symbol),
            asset: reward.symbol.clone(),
            amount: atoms,
            decimals: reward.decimals,
            counterparty: gauge_address.to_string(),
            fields: vec![
                field("Token contract", reward.address.clone()),
                field("Spender", format!("{gauge_address} (Quainance gauge)")),
                field("Allowance", format!("exactly {} {}", crate::amount::format_amount(atoms, reward.decimals), reward.symbol)),
            ],
            warnings: vec![],
            detail: serde_json::json!({"token": reward.address, "purpose": "incentivize", "spender": gauge_address.to_string()}),
            max_gas: 120_000,
            max_fee: self.parse_fee_cap(max_fee, crate::amount::QUAI_DECIMALS)?,
        })
        .await
    }

    /// Review funding a pool's rewards: `amount` of `token` streamed over `days`.
    ///
    /// This is the one action here that gives value away rather than moving it between the user's
    /// own places, so the review says so plainly and states the rate it implies.
    pub async fn review_incentivize(
        &mut self,
        account: Option<&str>,
        pair: &str,
        token: &str,
        value: &str,
        days: u32,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        let (gauge_address, pool) = self.gauge_pool(pair).await?;
        let reward = self.reward_asset(&pool, token).await?;
        let atoms = crate::amount::parse_amount(value, reward.decimals)?;
        if atoms.is_zero() {
            return Err(CoreError::Invalid("amount must be greater than zero".into()));
        }
        if !(1..=365).contains(&days) {
            return Err(CoreError::Invalid("reward duration must be between 1 and 365 days".into()));
        }
        // Rewards accrue per staked LP, so an empty gauge has nobody to pay and the call reverts.
        if pool.total_staked.is_zero() {
            return Err(CoreError::Rejected("nothing is staked in this pool yet, so it cannot be incentivised — stake LP first".into()));
        }
        let from = self.account(account)?;
        let owner = addr(&from.address)?;
        let erc = quai_sdk::contracts::Erc20::new(addr(&reward.address)?, &self.node.provider)?;
        if erc.balance_of(owner, owner, BlockTag::Latest).await? < atoms {
            return Err(CoreError::Insufficient(format!("{} balance is too low", reward.symbol)));
        }
        if erc.allowance(owner, owner, gauge_address, BlockTag::Latest).await? < atoms {
            return Err(crate::error::approval_needed(&reward.address, "approve the reward token for the gauge first (step 1 of 2)"));
        }
        let duration = u64::from(days) * 86_400;
        let contract = Contract::new(gauge_address, interface()?, &self.node.provider);
        let call = contract.prepare(
            "notifyRewardAmount",
            &[json!(pool.pid.to_string()), json!(reward.address), json!(atoms.to_string()), json!(duration.to_string())],
            U256::ZERO,
        )?;
        let call = crate::data::with_access_list(&self.node.provider, owner, call).await?;
        let per_day = crate::amount::to_f64(atoms, reward.decimals) / f64::from(days);
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: "incentivize".into(),
            title: format!("Fund {} rewards on {pair}", reward.symbol),
            asset: reward.symbol.clone(),
            amount: atoms,
            decimals: reward.decimals,
            counterparty: gauge_address.to_string(),
            fields: vec![
                field("Pool", format!("{pair} (pid {})", pool.pid)),
                field("You give away", format!("{} {}", crate::amount::format_amount(atoms, reward.decimals), reward.symbol)),
                field("Streamed over", format!("{days} days ({per_day:.4} {}/day)", reward.symbol)),
                field("Paid to", "everyone staking LP in this pool, in proportion to their stake"),
            ],
            warnings: vec![
                "this gives the tokens away: they go to the pool's stakers, not back to you".into(),
                "the deployed gauge has no recover function, so funding cannot be undone".into(),
            ],
            detail: serde_json::json!({
                "pair": pair, "pid": pool.pid, "reward": reward.address, "amount": atoms.to_string(), "duration": duration,
                "financial_effects":[{"direction":"out","asset":reward.symbol,"token":reward.address,"decimals":reward.decimals,"amount":atoms.to_string(),"note":"irrevocable gauge reward funding"}],
            }),
            max_gas: gauge_gas(300_000, pool.rewards.len().max(1)),
            max_fee: self.parse_fee_cap(max_fee, crate::amount::QUAI_DECIMALS)?,
        })
        .await
    }

    /// Review claiming rewards, or claiming and unstaking everything (`exit`).
    pub async fn review_harvest(&mut self, account: Option<&str>, pair: &str, exit: bool, max_fee: Option<&str>) -> Result<Review> {
        self.review_harvest_in_gauge(account, pair, None, exit, max_fee).await
    }

    pub async fn review_harvest_in_gauge(
        &mut self,
        account: Option<&str>,
        pair: &str,
        selected: Option<&str>,
        exit: bool,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        let from = self.account(account)?;
        let owner = addr(&from.address)?;
        let target = self.stake_target_in_gauge(account, pair, selected).await?;
        let gauge_address = target.address;
        let tokens: Vec<String> = target.rewards.iter().map(|r| r.token.address.clone()).collect();
        if tokens.is_empty() {
            return Err(CoreError::NotFound("this pool has no reward tokens".into()));
        }
        if !exit && !target.has_rewards() {
            return Err(CoreError::NotFound("nothing to claim on this pool yet".into()));
        }
        if exit && target.staked.is_zero() {
            return Err(CoreError::NotFound("nothing staked to exit".into()));
        }
        let contract = target.contract(&self.node.provider)?;
        let list: Vec<Value> = tokens.iter().map(|t| json!(t)).collect();
        let method = if exit { "exit" } else { "getReward" };
        let call = contract.prepare(method, &[json!(target.pid.to_string()), Value::Array(list)], U256::ZERO)?;
        let call = crate::data::with_access_list(&self.node.provider, owner, call).await?;
        let mut fields = vec![field("Pool", target.pool_text(pair))];
        for reward in &target.rewards {
            fields.push(field("Claiming", reward.earned_text()));
        }
        if exit {
            fields.push(field("Unstaking", format!("{} LP (all of it)", crate::amount::format_amount(target.staked, 18))));
        }
        let mut effects: Vec<Value> = target
            .rewards
            .iter()
            .filter(|r| !r.earned.is_zero())
            .map(|r| {
                json!({
                    "direction":"in", "asset":r.token.symbol, "token":r.token.address, "decimals":r.token.decimals,
                    "amount":r.earned.to_string(), "estimated":true, "note":"accrued rewards; accrual may change before inclusion"
                })
            })
            .collect();
        if exit {
            effects.push(json!({"direction":"in", "asset":"LP", "token":pair, "decimals":18,
                "amount":target.staked.to_string(), "estimated":false, "note":"unstaked principal"}));
        }
        let first = target.rewards.iter().find(|r| !r.earned.is_zero()).or(target.rewards.first());
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: if exit { "exit".into() } else { "harvest".into() },
            title: if exit {
                format!("Exit the {}: unstake everything and claim", target.kind.label())
            } else {
                format!("Claim {} rewards", target.kind.label())
            },
            asset: first.map(|r| r.token.symbol.clone()).unwrap_or_else(|| "rewards".into()),
            amount: first.map(|r| r.earned).unwrap_or_default(),
            decimals: first.map_or(18, |r| r.token.decimals),
            counterparty: gauge_address.to_string(),
            fields,
            warnings: vec![],
            detail: serde_json::json!({"pair": pair, "pid": target.pid, "gauge": gauge_address.to_string(), "exit": exit, "kind": target.kind, "financial_effects":effects}),
            max_gas: gauge_gas(220_000, target.rewards.len()),
            max_fee: self.parse_fee_cap(max_fee, crate::amount::QUAI_DECIMALS)?,
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(symbol: &str, decimals: u8) -> PoolToken {
        PoolToken { address: format!("0x00{symbol}").to_lowercase(), symbol: symbol.into(), decimals }
    }

    fn u(text: &str) -> U256 {
        U256::from_str_radix(text, 10).unwrap()
    }

    /// The measurement that settled the scale. Both live streams land on round durations only if
    /// `rewardRate` carries 18 decimals of precision above the token's own.
    #[test]
    fn emission_reproduces_the_live_streams() {
        // pid 0: 13,000 WQUAI over 117 days.
        let rate = u("1286008230452674897119341563786008");
        let per_day = emission_per_day(rate, 18);
        assert!((per_day - 111.11).abs() < 0.01, "{per_day} WQUAI/day");
        assert!((13_000.0 / per_day - 117.0).abs() < 0.05, "funds last {} days", 13_000.0 / per_day);
        // pid 1: 20,000 WQUAI over exactly 30 days.
        let rate = u("7716049382716049382716049382716049");
        let per_day = emission_per_day(rate, 18);
        assert!((per_day - 666.67).abs() < 0.01, "{per_day} WQUAI/day");
        assert!((20_000.0 / per_day - 30.0).abs() < 0.01, "funds last {} days", 20_000.0 / per_day);
        // A dead stream emits nothing.
        assert_eq!(emission_per_day(U256::ZERO, 18), 0.0);
    }

    #[test]
    fn emission_matches_pinned_runtime_for_six_eight_and_eighteen_decimals() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!("../tests/fixtures/gauge_rate_evidence.json")).unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let decimals = case["decimals"].as_u64().unwrap() as u8;
            let rate = u(case["reward_rate"].as_str().unwrap());
            let per_day = emission_per_day(rate, decimals);
            assert!((per_day - 100.0).abs() < 1e-10, "{decimals} decimals emitted {per_day}");
        }
    }

    fn pid0(now: u64) -> GaugePool {
        GaugePool {
            pid: 0,
            lp_token: "0x00602f12ea0491f02865aa6c418815319e2a645b".into(),
            total_staked: U256::from(7_574u64) * U256::from(10u128.pow(18)),
            // 7,574 of 360,684 LP staked: the live 2.1% share.
            lp_supply: U256::from(360_684u64) * U256::from(10u128.pow(18)),
            staked: U256::ZERO,
            rewards: vec![RewardStream {
                token: token("WQUAI", 18),
                rate: u("1286008230452674897119341563786008"),
                period_finish: now + 55 * 86_400,
                earned: U256::ZERO,
            }],
        }
    }

    /// The end-to-end figure, against the live pool: ~24% on $1,459 of staked WQI/WQUAI.
    #[test]
    fn apr_matches_the_live_pool() {
        let now = 1_789_600_000;
        let pool = pid0(now);
        let price = |_: &PoolToken| Some(0.008_599_188_048_956_24);
        let apr = pool.apr(now, &price, Some(1_459.57)).unwrap();
        assert!((23.0..25.0).contains(&apr), "{apr}% should be about 24%");
        assert_eq!(apr_text(Some(apr)), format!("{apr:.1}%"));
    }

    #[test]
    fn apr_is_absent_rather_than_zero_when_it_cannot_be_derived() {
        let now = 1_789_600_000;
        let pool = pid0(now);
        let priced = |_: &PoolToken| Some(0.0086);
        let unpriced = |_: &PoolToken| None;
        // Nothing staked, no denominator.
        assert_eq!(pool.apr(now, &priced, None), None);
        assert_eq!(pool.apr(now, &priced, Some(0.0)), None);
        // No price for the reward token.
        assert_eq!(pool.apr(now, &unpriced, Some(1_000.0)), None);
        // The stream has ended: it pays nothing, so there is no rate to annualise.
        let ended = GaugePool { rewards: vec![RewardStream { period_finish: now - 1, ..pool.rewards[0].clone() }], ..pool.clone() };
        assert_eq!(ended.apr(now, &priced, Some(1_000.0)), None);
        assert!(!ended.active(now));
        assert_eq!(apr_text(None), "—", "never 0%");
    }

    #[test]
    fn a_pool_reports_what_is_claimable_and_when_it_ends() {
        let now = 1_789_600_000;
        let mut pool = pid0(now);
        assert!(pool.active(now) && !pool.has_rewards());
        assert_eq!(pool.rewards[0].period_text(now), "ends in 55d");
        pool.rewards[0].earned = U256::from(12_400_000_000_000_000_000u128);
        assert!(pool.has_rewards());
        assert_eq!(pool.rewards[0].earned_text(), "12.4 WQUAI");
        assert_eq!(pool.reward_addresses(), vec!["0x00wquai"]);
        // An ended stream says so rather than showing a countdown.
        pool.rewards[0].period_finish = now - 10;
        assert_eq!(pool.rewards[0].period_text(now), "ended");
        assert!(!pool.rewards[0].live(now));
    }

    /// Several reward tokens sum, and an unpriced one does not silently zero the rest.
    #[test]
    fn multi_reward_pools_add_up() {
        let now = 1_789_600_000;
        let mut pool = pid0(now);
        pool.rewards.push(RewardStream {
            token: token("USDT", 6),
            rate: pool.rewards[0].rate,
            period_finish: now + 86_400,
            earned: U256::ZERO,
        });
        let one = pid0(now).apr(now, &|_| Some(0.0086), Some(1_000.0)).unwrap();
        let both = pool.apr_estimate(now, &|t: &PoolToken| (t.symbol == "WQUAI").then_some(0.0086), Some(1_000.0)).unwrap();
        assert!(both.partial);
        assert!((both.percent - one).abs() < 1e-9);
        assert!(pool.apr(now, &|t| (t.symbol == "WQUAI").then_some(0.0086), Some(1_000.0)).is_none());
        let all = pool.apr(now, &|_| Some(0.0086), Some(1_000.0)).unwrap();
        assert!(all > one, "a second priced stream raises the APR");
    }

    #[test]
    fn the_gauge_abi_parses() {
        quai_sdk::abi::AbiInterface::from_human_readable(GAUGE_ABI).unwrap();
    }
}

#[cfg(test)]
mod trading_apr_regressions {
    use super::*;

    #[test]
    fn thin_gauge_uses_full_staked_value_and_marks_partial_rewards() {
        let stream = RewardStream {
            token: PoolToken { decimals: 18, ..PoolToken::default() },
            rate: U256::from(10u64).pow(U256::from(36)),
            period_finish: 100,
            earned: U256::ZERO,
        };
        let mut pool = GaugePool {
            total_staked: U256::from(149),
            lp_supply: U256::from(1_000_000),
            rewards: vec![stream.clone()],
            ..GaugePool::default()
        };
        let apr = pool.apr_from_tvl(1, &|_| Some(1.0), Some(100_000.0)).unwrap();
        assert!((apr / (86_400.0 * 365.0 / 14.9 * 100.0) - 1.0).abs() < 1e-12);
        pool.rewards.push(RewardStream { token: PoolToken { symbol: "unpriced".into(), ..stream.token.clone() }, ..stream });
        let prices = |t: &PoolToken| (t.symbol != "unpriced").then_some(1.0);
        assert!(pool.apr_from_tvl(1, &prices, Some(100_000.0)).is_none());
        let partial = pool.apr_estimate_from_tvl(1, &prices, Some(100_000.0)).unwrap();
        assert!(partial.partial && partial.text().starts_with("≥ "));
    }
}
