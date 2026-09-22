//! Quainance launch-zone gauges: genesis reward campaigns on launched tokens.
//!
//! A launched token's pair can carry a *genesis campaign* — a fixed reward budget escrowed by the
//! launcher and streamed to whoever stakes that pair's LP. This is a different contract from the
//! PoolGauge in `gauge.rs`: its pools are created by the launcher rather than enrolled by hand,
//! each carries an activation threshold and a deadline, and the reward token is the launched token
//! itself rather than one of the core gauge's three allowlisted assets.
//!
//! **The pairs are the same pairs.** A zone gauge's `lpToken` is an ordinary Quainance pair from
//! the factory this wallet already trades and provides liquidity on, so LP added through the
//! wallet is exactly the LP these campaigns reward.
//!
//! Every signature and scale here was probed against chain 9 on 2026-09-17. Two deployments were
//! live at that point (the newer one carrying only its own launches), which is why the network
//! pins a list rather than one address.

use crate::amount;
use crate::markets::PoolToken;
use quai_sdk::U256;
use serde::{Deserialize, Serialize};

/// The zone gauge's surface, as deployed. The frontend ships a wider ABI; only what answered on
/// chain is declared here. The four writes share their signatures with the core gauge's, and their
/// selectors were found in both deployments' bytecode on 2026-09-17.
pub const ZONE_GAUGE_ABI: &[&str] = &[
    "function poolLength() view returns (uint256)",
    "function getPoolId(address pair) view returns (uint256 pid, bool exists)",
    "function poolSummary(uint256 pid) view returns (address lpToken, address launchToken, address creator, uint256 totalTokenSupply, uint256 poolTokenAmount, uint256 seedQuoteAmount, uint256 initialBurnedLiquidity, uint256 totalStaked, uint256 activationThreshold, uint64 activationReadyAt)",
    "function poolQuoteAsset(uint256 pid) view returns (address)",
    "function genesisCampaign(uint256 pid) view returns (uint256 totalReward, uint256 emittedReward, uint64 duration, uint64 activationDeadline, uint64 start, uint64 finish, bool activated, bool expired)",
    "function rewardTokenLength(uint256 pid) view returns (uint256)",
    "function rewardTokens(uint256 pid, uint256 index) view returns (address)",
    "function rewardState(uint256 pid, address rewardToken) view returns (uint64 periodFinish, uint256 currentRate, uint256 remainingReward, uint256 rewardPerToken, uint256 totalForfeited, uint256 retiredForfeited, uint256 totalPaid)",
    "function earned(uint256 pid, address user, address rewardToken) view returns (uint256)",
    "function balanceOf(uint256 pid, address user) view returns (uint256)",
    "function activationStakeBps() view returns (uint16)",
    "function registry() view returns (address)",
    "function factory() view returns (address)",
    "function stake(uint256 pid, uint256 amount)",
    "function withdraw(uint256 pid, uint256 amount)",
    "function getReward(uint256 pid, address[] tokens)",
    "function exit(uint256 pid, address[] tokens)",
];

/// Reward tokens a pool may carry at once.
pub const MAX_REWARD_TOKENS: u64 = 9;

/// One reward stream on a zone pool.
///
/// `rate` is the token's own atoms per second — not the extra-precision figure the core gauge
/// keeps. Measured: SMOL/WQI's stream reads 3.444664902998236331e18 per second, and 25,000,000
/// SMOL over its 7,257,600-second campaign is 3.4446649…e18 exactly.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ZoneReward {
    pub token: PoolToken,
    #[serde(with = "crate::explorer::u256_string")]
    pub rate: U256,
    /// Unix seconds the stream stops paying.
    pub period_finish: u64,
    /// Reward still to be paid out over the rest of the campaign.
    #[serde(with = "crate::explorer::u256_string")]
    pub remaining: U256,
    /// What these owners have earned and not claimed.
    #[serde(with = "crate::explorer::u256_string")]
    pub earned: U256,
}

impl ZoneReward {
    /// Still paying at `now`.
    pub fn live(&self, now: u64) -> bool {
        self.period_finish > now && !self.rate.is_zero()
    }

    /// Whole reward tokens emitted per day across the whole pool.
    pub fn per_day(&self) -> f64 {
        amount::to_f64(self.rate, self.token.decimals) * 86_400.0
    }

    /// `18.3M SMOL left`.
    pub fn remaining_text(&self) -> String {
        format!("{} {} left", amount::compact(amount::to_f64(self.remaining, self.token.decimals)), self.token.symbol)
    }

    /// `12.4 SMOL`.
    pub fn earned_text(&self) -> String {
        format!("{} {}", amount::format_amount_short(self.earned, self.token.decimals, 4), self.token.symbol)
    }
}

/// Where a campaign stands. The wallet never invents a state: each one is read from the pool.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Genesis {
    /// No campaign on this pool.
    #[default]
    None,
    /// Escrowed, waiting for enough LP to be staked before it starts paying.
    AwaitingActivation,
    /// Paying now.
    Live,
    /// Ran its course, or passed its activation deadline without starting.
    Ended,
}

impl Genesis {
    pub fn text(self) -> &'static str {
        match self {
            Genesis::None => "no campaign",
            Genesis::AwaitingActivation => "awaiting activation",
            Genesis::Live => "live",
            Genesis::Ended => "ended",
        }
    }
}

/// A launch-zone campaign: a fixed budget, escrowed, streamed to stakers over a fixed window.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Campaign {
    #[serde(with = "crate::explorer::u256_string")]
    pub total_reward: U256,
    #[serde(with = "crate::explorer::u256_string")]
    pub emitted_reward: U256,
    /// How long the stream runs once activated, in seconds.
    pub duration: u64,
    /// After this, an unactivated campaign can no longer start.
    pub activation_deadline: u64,
    pub start: u64,
    pub finish: u64,
    pub activated: bool,
    pub expired: bool,
}

impl Campaign {
    /// Whether this pool has a campaign at all.
    pub fn exists(&self) -> bool {
        !self.total_reward.is_zero()
    }

    pub fn state(&self, now: u64) -> Genesis {
        if !self.exists() {
            Genesis::None
        } else if self.expired || (self.activated && self.finish <= now) {
            Genesis::Ended
        } else if self.activated {
            Genesis::Live
        } else if self.activation_deadline != 0 && self.activation_deadline <= now {
            Genesis::Ended
        } else {
            Genesis::AwaitingActivation
        }
    }

    /// How much of the budget has been paid out, in basis points.
    pub fn emitted_bps(&self) -> u64 {
        crate::liquidity::share_bps(self.emitted_reward, self.total_reward)
    }
}

/// One launch-zone pool.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ZonePool {
    /// The gauge holding it (lowercase), since several are live at once.
    pub gauge: String,
    /// The factory that gauge builds its pairs from; pairs from a factory this network does not
    /// pin are readable but are not pools this wallet can add liquidity to.
    #[serde(default)]
    pub factory: String,
    pub pid: u64,
    /// The pair contract, which is also the staked LP token (lowercase).
    pub lp_token: String,
    /// The launched token this pool was created for.
    pub launch_token: String,
    /// The asset it was paired against (WQI or WQUAI).
    pub quote_asset: String,
    /// LP staked by everyone.
    #[serde(with = "crate::explorer::u256_string")]
    pub total_staked: U256,
    /// LP that must be staked before the campaign starts paying.
    #[serde(with = "crate::explorer::u256_string")]
    pub activation_threshold: U256,
    /// The pair's whole LP supply, so a share (and an APR) can be derived.
    #[serde(with = "crate::explorer::u256_string")]
    pub lp_supply: U256,
    /// LP staked here by this wallet's accounts.
    #[serde(with = "crate::explorer::u256_string")]
    pub staked: U256,
    pub campaign: Campaign,
    pub rewards: Vec<ZoneReward>,
}

impl ZonePool {
    /// Anything claimable right now.
    pub fn has_rewards(&self) -> bool {
        self.rewards.iter().any(|r| !r.earned.is_zero())
    }

    /// Reward tokens, for `getReward` and `exit`.
    pub fn reward_addresses(&self) -> Vec<String> {
        self.rewards.iter().map(|r| r.token.address.clone()).collect()
    }

    /// Any stream still paying.
    pub fn active(&self, now: u64) -> bool {
        self.rewards.iter().any(|r| r.live(now))
    }

    /// Progress toward the stake that starts the campaign, in basis points (capped at 100%).
    pub fn activation_bps(&self) -> u64 {
        crate::liquidity::share_bps(self.total_staked, self.activation_threshold)
    }

    /// The share of the pair's LP staked here, in basis points.
    pub fn staked_share_bps(&self) -> u64 {
        crate::liquidity::share_bps(self.total_staked, self.lp_supply)
    }

    /// Annualised return on the LP staked here, in percent.
    ///
    /// `reward_usd` prices one whole reward token and `pool_tvl_usd` values the whole pair; the
    /// staked share turns the second into the value the streams are paid against. `None` when
    /// either is unknown — an APR that cannot be derived is shown as `—`, never as `0%`.
    pub fn apr(&self, now: u64, reward_usd: &dyn Fn(&PoolToken) -> Option<f64>, pool_tvl_usd: Option<f64>) -> Option<f64> {
        self.apr_estimate(now, reward_usd, pool_tvl_usd).filter(|v| !v.partial).map(|v| v.percent)
    }

    pub fn apr_estimate(
        &self,
        now: u64,
        reward_usd: &dyn Fn(&PoolToken) -> Option<f64>,
        pool_tvl_usd: Option<f64>,
    ) -> Option<crate::gauge::AprEstimate> {
        let staked_usd =
            pool_tvl_usd.zip(amount::ratio(self.total_staked, self.lp_supply)).map(|(tvl, share)| tvl * share).filter(|v| *v > 0.0)?;
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
        priced.then(|| yearly / staked_usd * 100.0).filter(|v| v.is_finite()).map(|percent| crate::gauge::AprEstimate { percent, partial })
    }

    /// `297,619 SMOL/day · 18.3M SMOL left`, for the stream that is actually paying.
    pub fn reward_text(&self, now: u64) -> Option<String> {
        let stream = self.rewards.iter().find(|r| r.live(now))?;
        Some(format!("{}/day · {}", amount::group_thousands(&format!("{:.0}", stream.per_day())), stream.remaining_text()))
    }
}

/// Every launch-zone pool this wallet can see, across the gauges the network pins.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ZoneView {
    /// Gauges read (lowercase).
    pub gauges: Vec<String>,
    pub pools: Vec<ZonePool>,
    /// Gauges whose pool list was longer than [`MAX_ZONE_POOLS`]. Empty when all of them fit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted: Vec<crate::markets::Omitted>,
}

/// Pools one launch-zone gauge read covers, newest pid first. Campaigns are appended as tokens
/// launch, so a ceiling that kept the oldest would hide every new one.
pub const MAX_ZONE_POOLS: u64 = 256;

/// Without a Multicall3 each field is its own call, so the ceiling is much lower.
pub const MAX_ZONE_POOLS_SEQUENTIAL: u64 = 64;

impl ZoneView {
    /// The campaign on a pair, when it has one.
    pub fn pool_for(&self, pair: &str) -> Option<&ZonePool> {
        self.pools.iter().find(|p| p.lp_token.eq_ignore_ascii_case(pair))
    }
}

// ------------------------------------------------------------------------ reading the gauges

use crate::chain::{addr, address_at, is_zero_address, uint};
use crate::data::{DataCtx, READ_CALLER};
use crate::error::{CoreError, Result};
use quai_sdk::abi::AbiInterface;
use quai_sdk::contracts::Contract;
use quai_sdk::{BlockTag, QuaiAddress};
use serde_json::{Value, json};

pub(crate) fn interface() -> Result<AbiInterface> {
    crate::chain::interface(ZONE_GAUGE_ABI)
}

fn small(values: &[Value], index: usize) -> u64 {
    u64::try_from(uint(values, index)).unwrap_or(0)
}

fn flag(values: &[Value], index: usize) -> bool {
    values.get(index).and_then(Value::as_bool).unwrap_or_else(|| !uint(values, index).is_zero())
}

/// Read every pinned launch-zone gauge: its pools, their campaigns, and what these owners have
/// staked and earned. A gauge that fails verification or cannot be read is skipped rather than
/// failing the others — they are independent deployments.
pub async fn open(ctx: &DataCtx, owners: &[String]) -> Result<ZoneView> {
    let pins = ctx.network.ecosystem.zone_gauges.clone();
    if pins.is_empty() {
        return Err(CoreError::NotFound(format!("no launch-zone gauge on {}", ctx.network.name)));
    }
    let mut view = ZoneView::default();
    for pin in &pins {
        let Ok(address) = ctx.verify_pinned(pin, "Quainance launch-zone gauge").await else { continue };
        let lowercase = address.to_string().to_lowercase();
        match read_gauge(ctx, address, &lowercase, owners, None).await {
            Ok((mut pools, omitted)) => {
                view.gauges.push(lowercase);
                view.pools.append(&mut pools);
                view.omitted.extend(omitted);
            }
            Err(_) => continue,
        }
    }
    if view.gauges.is_empty() {
        return Err(CoreError::Network("no launch-zone gauge could be read".into()));
    }
    Ok(view)
}

/// Resolve a known pair across pinned deployments without applying discovery ceilings.
pub(crate) async fn for_pair_in_gauge(ctx: &DataCtx, owners: &[String], pair: &str, selected: Option<&str>) -> Result<ZoneView> {
    let mut view = ZoneView::default();
    for pin in &ctx.network.ecosystem.zone_gauges {
        if selected.is_some_and(|s| !s.eq_ignore_ascii_case(&pin.address)) {
            continue;
        }
        let address = ctx.verify_pinned(pin, "launch-zone gauge").await?;
        let address_text = address.to_string().to_lowercase();
        let (mut pools, _) = read_gauge(ctx, address, &address_text, owners, Some(pair)).await?;
        view.gauges.push(address_text);
        view.pools.append(&mut pools);
    }
    Ok(view)
}

async fn read_gauge(
    ctx: &DataCtx,
    address: QuaiAddress,
    lowercase: &str,
    owners: &[String],
    pair: Option<&str>,
) -> Result<(Vec<ZonePool>, Option<crate::markets::Omitted>)> {
    if pair.is_none()
        && let Some(read) = batched_gauge(ctx, lowercase, owners).await
    {
        return Ok(read);
    }
    // No Multicall3: read level by level. Correct, just a call per value.
    let gauge = Contract::new(address, interface()?, &ctx.node.provider);
    let caller = addr(READ_CALLER)?;
    // Which factory this deployment builds pairs from is recorded, not enforced: the deployments
    // run side by side and a newer one may serve a factory this network does not pin. What keeps
    // a foreign pair out of the screen is the matching that follows — a campaign is only ever
    // shown against a pair the wallet already lists — and the code-hash pin verified above.
    let factory = address_at(&gauge.call(caller, "factory", &[], BlockTag::Latest).await?, 0);
    let count = small(&gauge.call(caller, "poolLength", &[], BlockTag::Latest).await?, 0);
    let pids: Vec<u64> = if let Some(pair) = pair {
        let answer = gauge.call(caller, "getPoolId", &[json!(pair)], BlockTag::Latest).await?;
        if answer.get(1).and_then(Value::as_bool) != Some(true) {
            return Ok((vec![], None));
        }
        vec![u64::try_from(uint(&answer, 0)).map_err(|_| CoreError::Invalid("gauge pool id exceeds supported range".into()))?]
    } else {
        (0..count).rev().take(usize::try_from(MAX_ZONE_POOLS_SEQUENTIAL).unwrap_or(usize::MAX)).collect()
    };
    let omitted = omitted_pools(lowercase, count, pids.len());
    let mut pools = Vec::new();
    for pid in pids {
        let arg = json!(pid.to_string());
        let summary = gauge.call(caller, "poolSummary", std::slice::from_ref(&arg), BlockTag::Latest).await?;
        let lp_token = address_at(&summary, 0);
        if lp_token.is_empty() || is_zero_address(&lp_token) {
            return Err(CoreError::Network("gauge returned an invalid LP identity".into()));
        }
        if pair.is_some_and(|p| !p.eq_ignore_ascii_case(&lp_token)) {
            return Err(CoreError::Rejected("gauge pool id does not match the requested LP token".into()));
        }
        let genesis = gauge.call(caller, "genesisCampaign", std::slice::from_ref(&arg), BlockTag::Latest).await?;
        let campaign = Campaign {
            total_reward: uint(&genesis, 0),
            emitted_reward: uint(&genesis, 1),
            duration: small(&genesis, 2),
            activation_deadline: small(&genesis, 3),
            start: small(&genesis, 4),
            finish: small(&genesis, 5),
            activated: flag(&genesis, 6),
            expired: flag(&genesis, 7),
        };
        let mut staked = U256::ZERO;
        for owner in owners {
            let out = gauge.call(caller, "balanceOf", &[arg.clone(), json!(owner)], BlockTag::Latest).await?;
            staked = staked.saturating_add(uint(&out, 0));
        }
        let reward_count = small(&gauge.call(caller, "rewardTokenLength", std::slice::from_ref(&arg), BlockTag::Latest).await?, 0);
        let mut rewards = Vec::new();
        if reward_count > MAX_REWARD_TOKENS {
            return Err(CoreError::Invalid(
                "zone gauge reward list exceeds the supported bound; principal withdrawal remains available".into(),
            ));
        }
        for index in 0..reward_count {
            let out = gauge.call(caller, "rewardTokens", &[arg.clone(), json!(index.to_string())], BlockTag::Latest).await?;
            let token_address = address_at(&out, 0);
            if token_address.is_empty() || is_zero_address(&token_address) {
                continue;
            }
            let state = gauge.call(caller, "rewardState", &[arg.clone(), json!(token_address)], BlockTag::Latest).await?;
            let token = crate::markets::token_meta_required(ctx, &token_address).await?;
            let mut earned = U256::ZERO;
            for owner in owners {
                let out = gauge.call(caller, "earned", &[arg.clone(), json!(owner), json!(token.address)], BlockTag::Latest).await?;
                earned = earned.saturating_add(uint(&out, 0));
            }
            rewards.push(ZoneReward { token, rate: uint(&state, 1), period_finish: small(&state, 0), remaining: uint(&state, 2), earned });
        }
        let quote_asset =
            gauge.call(caller, "poolQuoteAsset", &[arg], BlockTag::Latest).await.map(|v| address_at(&v, 0)).unwrap_or_default();
        let lp_supply = Contract::new(
            addr(&lp_token)?,
            AbiInterface::from_human_readable(&["function totalSupply() view returns (uint256)"])
                .map_err(|e| CoreError::Invalid(format!("lp abi: {e}")))?,
            &ctx.node.provider,
        )
        .call(caller, "totalSupply", &[], BlockTag::Latest)
        .await
        .map(|v| uint(&v, 0))?;
        pools.push(ZonePool {
            gauge: lowercase.to_string(),
            factory: factory.clone(),
            pid,
            lp_token,
            launch_token: address_at(&summary, 1),
            quote_asset,
            total_staked: uint(&summary, 7),
            activation_threshold: uint(&summary, 8),
            lp_supply,
            staked,
            campaign,
            rewards,
        });
    }
    Ok((pools, omitted))
}

/// What a gauge's pool ceiling kept out, when it kept anything out.
fn omitted_pools(gauge: &str, total: u64, read: usize) -> Option<crate::markets::Omitted> {
    (usize::try_from(total).unwrap_or(usize::MAX) > read).then(|| crate::markets::Omitted {
        source: format!("launch-zone gauge {}", crate::session::short_address(gauge)),
        read,
        total: usize::try_from(total).unwrap_or(usize::MAX),
    })
}

/// One gauge in three batched rounds instead of a call per value: the gauge's own figures and
/// every pool's summary, campaign, quote asset and reward count; then each pair's LP supply and
/// each pool's reward tokens; then every stream's state and what these owners staked and earned.
/// Measured 2026-09-17: 13 pools read in 172 sequential calls before this.
async fn batched_gauge(ctx: &DataCtx, gauge: &str, owners: &[String]) -> Option<(Vec<ZonePool>, Option<crate::markets::Omitted>)> {
    use crate::multicall::{Arg, Call, Multicall, address_word, word};
    let mc = Multicall::open(ctx).await?;
    let head = mc.try_all(&[Call::view(gauge, "factory()", &[]), Call::view(gauge, "poolLength()", &[])]).await.ok()?;
    if head.len() != 2 || head.iter().any(|row| row.as_ref().is_none_or(|data| data.len() < 32)) {
        return None;
    }
    let factory = head.first().and_then(Option::as_ref).map(|d| address_word(d, 0)).unwrap_or_default();
    let count = head.get(1).and_then(Option::as_ref).map_or(0, |d| u64::try_from(word(d, 0)).unwrap_or(0));
    // Newest pid first, so that if the ceiling ever applies the campaigns kept are the new ones.
    let pids: Vec<u64> = (0..count).rev().take(usize::try_from(MAX_ZONE_POOLS).unwrap_or(usize::MAX)).collect();
    let omitted = omitted_pools(gauge, count, pids.len());
    let pid = |p: u64| Arg::Uint(U256::from(p));
    // Round 1: per pool, summary · campaign · quote asset · reward count · each owner's stake.
    let per_pool = 4 + owners.len();
    let mut calls = Vec::new();
    for &p in &pids {
        calls.push(Call::view(gauge, "poolSummary(uint256)", &[pid(p)]));
        calls.push(Call::view(gauge, "genesisCampaign(uint256)", &[pid(p)]));
        calls.push(Call::view(gauge, "poolQuoteAsset(uint256)", &[pid(p)]));
        calls.push(Call::view(gauge, "rewardTokenLength(uint256)", &[pid(p)]));
        for owner in owners {
            calls.push(Call::view(gauge, "balanceOf(uint256,address)", &[pid(p), Arg::Addr(owner.clone())]));
        }
    }
    let round1 = mc.try_all(&calls).await.ok()?;
    if round1.len() != calls.len() || round1.iter().any(|row| row.as_ref().is_none_or(|data| data.len() < 32)) {
        return None;
    }
    let mut drafts = Vec::new();
    let mut reward_counts = Vec::new();
    for (i, &p) in pids.iter().enumerate() {
        let at = |k: usize| round1.get(i * per_pool + k).and_then(Option::as_ref);
        let summary = at(0).filter(|d| d.len() >= 320)?;
        let lp_token = address_word(summary, 0);
        if lp_token.is_empty() || is_zero_address(&lp_token) {
            continue;
        }
        let genesis = at(1).filter(|d| d.len() >= 256)?;
        let campaign = Campaign {
            total_reward: word(genesis, 0),
            emitted_reward: word(genesis, 1),
            duration: u64::try_from(word(genesis, 2)).unwrap_or(0),
            activation_deadline: u64::try_from(word(genesis, 3)).unwrap_or(0),
            start: u64::try_from(word(genesis, 4)).unwrap_or(0),
            finish: u64::try_from(word(genesis, 5)).unwrap_or(0),
            activated: !word(genesis, 6).is_zero(),
            expired: !word(genesis, 7).is_zero(),
        };
        let staked = (0..owners.len()).fold(U256::ZERO, |sum, o| sum.saturating_add(at(4 + o).map_or(U256::ZERO, |d| word(d, 0))));
        drafts.push(ZonePool {
            gauge: gauge.to_string(),
            factory: factory.clone(),
            pid: p,
            launch_token: address_word(summary, 1),
            total_staked: word(summary, 7),
            activation_threshold: word(summary, 8),
            quote_asset: at(2).map(|d| address_word(d, 0)).unwrap_or_default(),
            lp_token,
            staked,
            campaign,
            rewards: Vec::new(),
            lp_supply: U256::ZERO,
        });
        let reward_count = u64::try_from(word(at(3)?, 0)).ok()?;
        if reward_count > MAX_REWARD_TOKENS {
            return None;
        }
        reward_counts.push(reward_count);
    }
    // Round 2: each pair's LP supply and each pool's reward-token addresses.
    let mut calls = Vec::new();
    for (pool, n) in drafts.iter().zip(&reward_counts) {
        calls.push(Call::view(&pool.lp_token, "totalSupply()", &[]));
        for index in 0..*n {
            calls.push(Call::view(gauge, "rewardTokens(uint256,uint256)", &[pid(pool.pid), Arg::Uint(U256::from(index))]));
        }
    }
    let round2 = mc.try_all(&calls).await.ok()?;
    if round2.len() != calls.len() || round2.iter().any(|row| row.as_ref().is_none_or(|data| data.len() < 32)) {
        return None;
    }
    let mut i = 0;
    let mut tokens: Vec<Vec<String>> = Vec::new();
    for (pool, n) in drafts.iter_mut().zip(&reward_counts) {
        pool.lp_supply = round2.get(i).and_then(Option::as_ref).map_or(U256::ZERO, |d| word(d, 0));
        i += 1;
        let mut list = Vec::new();
        for _ in 0..*n {
            if let Some(a) =
                round2.get(i).and_then(Option::as_ref).map(|d| address_word(d, 0)).filter(|a| !a.is_empty() && !is_zero_address(a))
            {
                list.push(a);
            }
            i += 1;
        }
        tokens.push(list);
    }
    // Round 3: every stream's state, and what each owner has earned on it.
    let mut calls = Vec::new();
    for (pool, list) in drafts.iter().zip(&tokens) {
        for token in list {
            calls.push(Call::view(gauge, "rewardState(uint256,address)", &[pid(pool.pid), Arg::Addr(token.clone())]));
            for owner in owners {
                calls.push(Call::view(
                    gauge,
                    "earned(uint256,address,address)",
                    &[pid(pool.pid), Arg::Addr(owner.clone()), Arg::Addr(token.clone())],
                ));
            }
        }
    }
    let round3 = mc.try_all(&calls).await.ok()?;
    if round3.len() != calls.len() || round3.iter().any(|row| row.as_ref().is_none_or(|data| data.len() < 32)) {
        return None;
    }
    let mut i = 0;
    for (pool, list) in drafts.iter_mut().zip(&tokens) {
        let mut rewards = Vec::with_capacity(list.len());
        for address in list {
            let state = round3.get(i).and_then(Option::as_ref).filter(|d| d.len() >= 224)?.clone();
            i += 1;
            let mut earned = U256::ZERO;
            for _ in owners {
                earned = earned.saturating_add(round3.get(i).and_then(Option::as_ref).map_or(U256::ZERO, |d| word(d, 0)));
                i += 1;
            }
            rewards.push(ZoneReward {
                token: crate::markets::token_meta_required(ctx, address).await.ok()?,
                period_finish: u64::try_from(word(&state, 0)).unwrap_or(0),
                rate: word(&state, 1),
                remaining: word(&state, 2),
                earned,
            });
        }
        pool.rewards = rewards;
    }
    Some((drafts, omitted))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e18(n: u128) -> U256 {
        U256::from(n) * U256::from(10u128.pow(18))
    }

    fn token(symbol: &str) -> PoolToken {
        PoolToken { address: "0x00033a4d7dcb5559e96245a90dbe6f858e10e8bc".into(), symbol: symbol.into(), decimals: 18 }
    }

    /// The figures a campaign quotes are the ones the pool reports, at the scale it reports them.
    /// Measured against SMOL/WQI on 2026-09-17: 25,000,000 SMOL over 7,257,600 s reads as a rate
    /// of 3.444664902998236331e18, which is 297,619 a day — the same figure Quainance shows.
    #[test]
    fn a_stream_is_read_at_the_scale_the_gauge_keeps_it() {
        let stream = ZoneReward {
            token: token("SMOL"),
            rate: U256::from(3_444_664_902_998_236_331u128),
            period_finish: 1_794_926_718,
            remaining: e18(18_295_393),
            earned: U256::ZERO,
        };
        assert_eq!(format!("{:.0}", stream.per_day()), "297619");
        assert!(stream.live(1_789_600_000) && !stream.live(1_794_926_719));
        assert!(stream.remaining_text().contains("SMOL left"), "{}", stream.remaining_text());
    }

    /// A campaign's state is read, never guessed: escrowed but unstaked is not the same as paying,
    /// and a deadline that passed without activation is over rather than pending forever.
    #[test]
    fn a_campaign_says_where_it_stands() {
        let now = 1_789_600_000u64;
        let base = Campaign {
            total_reward: e18(25_000_000),
            emitted_reward: e18(6_704_606),
            duration: 7_257_600,
            activation_deadline: now + 1_000,
            start: now - 100,
            finish: now + 5_000_000,
            activated: true,
            expired: false,
        };
        assert_eq!(base.state(now), Genesis::Live);
        assert_eq!(base.emitted_bps(), 2681, "26.81% of the budget paid");
        assert_eq!(Campaign { activated: false, ..base.clone() }.state(now), Genesis::AwaitingActivation);
        // The deadline passing without activation ends it; so does the stream reaching its finish.
        let missed = Campaign { activated: false, activation_deadline: now - 1, ..base.clone() };
        assert_eq!(missed.state(now), Genesis::Ended);
        assert_eq!(Campaign { finish: now - 1, ..base.clone() }.state(now), Genesis::Ended);
        assert_eq!(Campaign { expired: true, ..base }.state(now), Genesis::Ended);
        assert_eq!(Campaign::default().state(now), Genesis::None, "no budget is no campaign");
    }

    /// Activation is progress toward a stake, and the APR is quoted against the value actually
    /// staked — not the whole pair, of which only a share is in the gauge.
    #[test]
    fn activation_and_apr_come_from_what_is_staked() {
        let pool = ZonePool {
            total_staked: e18(48_623),
            activation_threshold: e18(270),
            lp_supply: e18(385_022),
            rewards: vec![ZoneReward {
                token: token("SMOL"),
                rate: U256::from(3_444_664_902_998_236_331u128),
                period_finish: 1_794_926_718,
                remaining: e18(18_295_393),
                earned: U256::ZERO,
            }],
            ..ZonePool::default()
        };
        assert_eq!(pool.activation_bps(), 10_000, "far past the threshold, capped at 100%");
        assert_eq!(pool.staked_share_bps(), 1262, "12.62% of the pair's LP is staked");
        // 297,619 SMOL a day at $0.001 is ~$108,631 a year, paid against the 12.62% of a $1,200
        // pair that is actually staked ($151) — not against the whole pair, which would quote a
        // tenth of the return.
        let apr = pool.apr(1_789_600_000, &|_| Some(0.001), Some(1_200.0)).unwrap();
        assert!((apr - 71_732.0).abs() < 500.0, "{apr}");
        // Unpriced rewards, an unknown TVL or nothing staked give no figure at all.
        assert_eq!(pool.apr(1_789_600_000, &|_| None, Some(1_200.0)), None);
        assert_eq!(pool.apr(1_789_600_000, &|_| Some(0.001), None), None);
        assert_eq!(ZonePool::default().apr(1_789_600_000, &|_| Some(1.0), Some(10.0)), None);
        // A campaign that has ended pays nothing, whatever its rate says.
        assert_eq!(pool.apr(1_794_926_719, &|_| Some(0.001), Some(1_200.0)), None);
    }
}
