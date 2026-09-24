//! Quainance's bonding curve: where a launched token stands on its curve, and trading on it.
//!
//! Every launch gets its own curve contract (a market) that sells from a fixed allocation until a
//! QUAI target is raised, then graduates into an AMM pair. The contracts are not pinned one by one
//! — each carries its token and parameters in its bytecode, so no two hash alike — but they are
//! all created by one launcher, which is. A curve is only used when the pinned launcher names it as
//! the token's market, it names that token and launcher back, and it has not graduated: the same
//! checks Quainance's own site makes before it lets anyone trade.
//!
//! Probed against chain 9 on 2026-09-17. Buying pays native QUAI. Selling needs an exact token
//! approval to the curve, and credits the proceeds rather than sending them: they are collected
//! with `claimQuote`. A buy that overshoots the graduation target credits the excess the same way.

use crate::chain::{addr, interface};
use crate::data::{DataCtx, READ_CALLER};
use crate::error::{CoreError, Result, approval_needed};
use crate::multicall::{Arg, Call, Multicall, word};
use crate::network::PinnedContract;
use crate::session::Session;
use crate::tx::{AccountRequest, Review, field};
use quai_sdk::contracts::{Contract, Erc20};
use quai_sdk::{BlockTag, QuaiAddress, U256};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The launcher's view of a token, and a curve's own surface, as deployed.
/// Where the Quainance launcher keeps `launches(token)`: a mapping at slot 0 of structs whose second
/// word is `market`, the curve. Read against `launches()` at the same block on mainnet
/// (2026-09-24, Q9000 and SOAP); the live test `a_bonding_curve_reads_verifies_and_simulates`
/// reads it again.
pub const LAUNCHES_SLOT: u64 = 0;
pub const LAUNCH_MARKET_FIELD: u64 = 1;

pub const LAUNCHER_ABI: &[&str] = &[
    "function launches(address token) view returns (address creator, address market, address pair, uint256 curveTokenAmount, uint256 poolTokenAmount, uint256 incentiveAmount, uint64 campaignDuration, uint256 gaugePoolId, bool graduated)",
];
pub const CURVE_ABI: &[&str] = &[
    "function token() view returns (address)",
    "function launcher() view returns (address)",
    "function graduated() view returns (bool)",
    "function quoteBuy(uint256 grossQuoteOffered) view returns (uint256 grossQuoteUsed, uint256 netQuoteUsed, uint256 feeAmount, uint256 tokenAmount, uint256 excessCredit, bool reachesThreshold)",
    "function quoteSell(uint256 maximumTokenAmount) view returns (uint256 tokenAmountUsed, uint256 grossQuoteAmount, uint256 feeAmount, uint256 quoteCredit)",
    "function claimableQuote(address account) view returns (uint256)",
    "function claimQuote(address recipient) returns (uint256 amount)",
    "function buy(uint256 minimumTokenAmount, uint256 deadline) payable returns (uint256 tokenAmount, uint256 excessCredit)",
    "function sell(uint256 maximumTokenAmount, uint256 minimumQuoteAmount, uint256 deadline) returns (uint256 quoteCredit)",
];

/// Points the price curve is drawn from.
pub const CURVE_POINTS: usize = 48;

#[derive(Clone, Debug, Serialize)]
pub struct CurveTradeQuote {
    pub family: crate::capabilities::Family,
    pub token: crate::markets::PoolToken,
    pub curve: String,
    pub owner: String,
    pub sell: bool,
    pub input: String,
    pub expected_output: String,
    pub minimum_output: String,
    pub maximum_fee: String,
    pub excess_credit: Option<String>,
    pub immediate_native_payment: bool,
    pub onchain_deadline: bool,
    pub observed_at: u64,
}

/// A token on its bonding curve, read from the curve itself.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct CurveMarket {
    #[serde(default)]
    pub token_decimals: u8,
    /// Token (lowercase) and the curve selling it.
    pub token: String,
    pub curve: String,
    /// Tokens the curve sells before graduating, and how many it has sold.
    #[serde(with = "crate::explorer::u256_string")]
    pub curve_tokens: U256,
    #[serde(with = "crate::explorer::u256_string")]
    pub tokens_sold: U256,
    /// QUAI raised (after fees) and the target that graduates the curve.
    #[serde(with = "crate::explorer::u256_string")]
    pub raised: U256,
    #[serde(with = "crate::explorer::u256_string")]
    pub target: U256,
    /// Curve fee on each trade, in basis points.
    pub fee_bps: u64,
    /// QUAI per whole token at the current point on the curve.
    pub spot_price: f64,
    pub progress_bps: u64,
    pub graduated: bool,
    /// The curve as (QUAI raised, marginal price in QUAI) from launch to graduation.
    pub points: Vec<(f64, f64)>,
    /// These owners' token balance, and QUAI credited to them (sales, overshoot) awaiting a claim.
    #[serde(with = "crate::explorer::u256_string")]
    pub held: U256,
    #[serde(with = "crate::explorer::u256_string")]
    pub claimable: U256,
}

impl CurveMarket {
    /// Whole QUAI raised and the target.
    pub fn raised_quai(&self) -> f64 {
        crate::amount::to_f64(self.raised, crate::amount::QUAI_DECIMALS)
    }
    pub fn target_quai(&self) -> f64 {
        crate::amount::to_f64(self.target, crate::amount::QUAI_DECIMALS)
    }
    /// The share of the curve's allocation sold, in basis points.
    pub fn sold_bps(&self) -> u64 {
        crate::liquidity::share_bps(self.tokens_sold, self.curve_tokens)
    }
}

/// The marginal price between successive samples of `cumulativeTokensSold`: QUAI raised over
/// tokens sold across each step, at the step's midpoint.
pub fn price_points(target: f64, sold: &[f64]) -> Vec<(f64, f64)> {
    let n = sold.len().saturating_sub(1).max(1) as f64;
    sold.windows(2)
        .enumerate()
        .filter_map(|(i, w)| {
            let tokens = w[1] - w[0];
            (tokens > 0.0).then(|| ((i as f64 + 0.5) * target / n, (target / n) / tokens))
        })
        .collect()
}

/// The pinned launcher that launched this curve: the curve names it, and only a pinned launcher is
/// believed. Quainance runs two (the launch zone's and the revenue system's), with one interface.
async fn launcher_pin<'a>(
    network: &'a crate::network::NetworkProfile,
    node: &crate::network::Node,
    curve: &str,
) -> Result<&'a PinnedContract> {
    let eco = &network.ecosystem;
    let pins: Vec<&PinnedContract> = [eco.curve_launcher.as_ref(), eco.revenue_curve_launcher.as_ref()].into_iter().flatten().collect();
    if pins.is_empty() {
        return Err(CoreError::NotFound(format!("no bonding-curve launcher on {}", network.name)));
    }
    let said = Contract::new(addr(curve)?, interface(CURVE_ABI)?, &node.provider)
        .call(addr(READ_CALLER)?, "launcher", &[], BlockTag::Latest)
        .await?;
    let said = said.first().and_then(|v| v.as_str()).unwrap_or_default().to_lowercase();
    pins.into_iter()
        .find(|p| p.address.eq_ignore_ascii_case(&said))
        .ok_or_else(|| CoreError::Rejected(format!("{curve} names a launcher this wallet does not know ({said}); refusing to use it")))
}

/// Check a curve against the pinned launcher; returns its address when it may be traded or read.
///
/// The launcher is the wallet's only reason to believe a curve address belongs to a token, and a
/// curve is the destination of every buy, sell and claim, so a review passes
/// [`crate::data::Trust::FirstHand`] and the launcher's code is read rather than remembered.
async fn verified_curve(
    app: &crate::appdb::AppDb,
    node: &crate::network::Node,
    network: &crate::network::NetworkProfile,
    token: &str,
    curve: &str,
    trust: crate::data::Trust,
) -> Result<QuaiAddress> {
    let launcher =
        crate::data::verify_pinned(app, node, network, launcher_pin(network, node, curve).await?, "Quainance curve launcher", trust)
            .await?;
    let caller = addr(READ_CALLER)?;
    let named = Contract::new(launcher, interface(LAUNCHER_ABI)?, &node.provider)
        .call(caller, "launches", &[json!(token)], BlockTag::Latest)
        .await?;
    let market = named.get(1).and_then(|v| v.as_str()).unwrap_or_default().to_lowercase();
    if !market.eq_ignore_ascii_case(curve) {
        return Err(CoreError::Rejected(format!("the Quainance launcher does not name {curve} as this token's curve; refusing to use it")));
    }
    // That answer is a call result: the node's word. The launcher's own storage, proven at a block
    // the network's RPC confirms when a monitor serves the reads, is not. The curve is where a buy
    // sends QUAI, so on a review it is taken from the proof.
    if !trust.may_cache() {
        let slot = crate::anchor::mapping_field_slot(addr(token)?, LAUNCHES_SLOT, LAUNCH_MARKET_FIELD);
        if let Some((proven, _)) = crate::anchor::prove_state(node, network, &[(launcher, &[slot])], "Quainance curve").await? {
            let stored = proven[0].storage_value(slot).map(crate::anchor::word_address).unwrap_or_default();
            if !stored.eq_ignore_ascii_case(curve) {
                return Err(CoreError::Rejected(format!(
                    "the Quainance launcher's own records do not name {curve} as this token's curve; refusing to use it"
                )));
            }
        }
    }
    let address = addr(curve)?;
    let c = Contract::new(address, interface(CURVE_ABI)?, &node.provider);
    let text = |v: Vec<serde_json::Value>| v.first().and_then(|x| x.as_str()).unwrap_or_default().to_lowercase();
    if !text(c.call(caller, "token", &[], BlockTag::Latest).await?).eq_ignore_ascii_case(token)
        || !text(c.call(caller, "launcher", &[], BlockTag::Latest).await?).eq_ignore_ascii_case(&launcher.to_string())
    {
        return Err(CoreError::Rejected("this curve does not name its token and launcher back; refusing to use it".into()));
    }
    Ok(address)
}

/// A curve's state, its price curve and what these owners hold and are owed, in one batched read.
pub async fn market(ctx: &DataCtx, token: &str, curve: &str, owners: &[String]) -> Result<CurveMarket> {
    ctx.online()?;
    if crate::hartii_tx::matches_curve_runtime(ctx, curve).await? {
        return crate::hartii_tx::market(ctx, token, curve, owners).await;
    }
    let token = token.to_lowercase();
    let token_decimals = crate::markets::token_meta_required(ctx, &token).await?.decimals;
    let curve = verified_curve(&ctx.app, &ctx.node, &ctx.network, &token, curve, ctx.trust).await?.to_string().to_lowercase();
    let mc = Multicall::open(ctx).await.ok_or_else(|| CoreError::NotFound("reading a curve needs Multicall3 on this network".into()))?;
    let state = [
        "curveTokenAmount()",
        "tokensSold()",
        "netQuoteRaised()",
        "graduationQuoteAmount()",
        "feeBps()",
        "spotPriceQuoteX18()",
        "graduationProgressBps()",
        "graduated()",
    ];
    let mut calls: Vec<Call> = state.iter().map(|s| Call::view(&curve, s, &[])).collect();
    for owner in owners {
        calls.push(Call::view(&token, "balanceOf(address)", &[Arg::Addr(owner.clone())]));
        calls.push(Call::view(&curve, "claimableQuote(address)", &[Arg::Addr(owner.clone())]));
    }
    let head = mc.try_all(&calls).await?;
    if head.len() != calls.len() || head.iter().any(|v| v.as_ref().is_none_or(|bytes| bytes.len() != 32)) {
        return Err(CoreError::Network("curve state is incomplete; balances and price are unavailable".into()));
    }
    let at = |i: usize| head.get(i).and_then(Option::as_ref).map_or(U256::ZERO, |d| word(d, 0));
    let (curve_tokens, tokens_sold, raised, target) = (at(0), at(1), at(2), at(3));
    let (mut held, mut claimable) = (U256::ZERO, U256::ZERO);
    for o in 0..owners.len() {
        held = held.saturating_add(at(state.len() + 2 * o));
        claimable = claimable.saturating_add(at(state.len() + 2 * o + 1));
    }
    // The curve itself: tokens sold at evenly spaced amounts raised, launch to graduation.
    let steps = (0..=CURVE_POINTS).map(|i| target * U256::from(i) / U256::from(CURVE_POINTS));
    let samples: Vec<Call> = steps.map(|q| Call::view(&curve, "cumulativeTokensSold(uint256)", &[Arg::Uint(q)])).collect();
    let sold: Vec<f64> = mc
        .try_all(&samples)
        .await?
        .iter()
        .map(|d| d.as_deref().map_or(f64::NAN, |d| crate::amount::to_f64(word(d, 0), token_decimals)))
        .collect();
    let target_quai = crate::amount::to_f64(target, crate::amount::QUAI_DECIMALS);
    Ok(CurveMarket {
        token_decimals,
        token,
        curve,
        curve_tokens,
        tokens_sold,
        raised,
        target,
        fee_bps: u64::try_from(at(4)).unwrap_or(0),
        spot_price: crate::amount::to_f64(at(5), 18),
        progress_bps: u64::try_from(at(6)).unwrap_or(0).min(10_000),
        graduated: !at(7).is_zero(),
        points: price_points(target_quai, &sold),
        held,
        claimable,
    })
}

/// What a curve pays for `input`: `(output, fee, credit back, hartii)`. A buy's input is QUAI and
/// its output tokens; a sell's the reverse. Both launchpads, each read through its own verified
/// curve; the fee is the curve's, and a Quainance buy past its target credits the excess back.
pub async fn quote_on_curve(
    ctx: &crate::data::DataCtx,
    caller: QuaiAddress,
    token: &str,
    curve: &str,
    input: U256,
    sell: bool,
) -> Result<(U256, U256, Option<String>, bool)> {
    let hartii = crate::hartii_tx::matches_curve_runtime(ctx, curve).await?;
    if hartii {
        let target = crate::hartii_tx::verified_curve(ctx, token, curve).await?;
        let contract = Contract::new(target.address, interface(crate::hartii::CURVE_ABI)?, &ctx.node.provider);
        if sell {
            let gross = crate::chain::uint(&contract.call(caller, "quoteSell", &[json!(input.to_string())], BlockTag::Latest).await?, 0);
            let (output, fee) = crate::hartii::after_fee(gross, target.fee_bps)?;
            return Ok((output, fee, None, true));
        }
        let (net, fee) = crate::hartii::after_fee(input, target.fee_bps)?;
        let raw = crate::chain::uint(&contract.call(caller, "quoteBuy", &[json!(net.to_string())], BlockTag::Latest).await?, 0);
        let output = if target.graduated {
            raw
        } else {
            let supply = crate::chain::uint(&contract.call(caller, "curveSupply", &[], BlockTag::Latest).await?, 0);
            let sold = crate::chain::uint(&contract.call(caller, "tokensSold", &[], BlockTag::Latest).await?, 0);
            raw.min(supply.checked_sub(sold).ok_or_else(|| CoreError::Invalid("curve sold exceeds allocation".into()))?)
        };
        return Ok((output, fee, None, true));
    }
    let address = verified_curve(&ctx.app, &ctx.node, &ctx.network, token, curve, ctx.trust).await?;
    let contract = Contract::new(address, interface(CURVE_ABI)?, &ctx.node.provider);
    let values = contract.call(caller, if sell { "quoteSell" } else { "quoteBuy" }, &[json!(input.to_string())], BlockTag::Latest).await?;
    Ok((crate::chain::uint(&values, 3), crate::chain::uint(&values, 2), (!sell).then(|| crate::chain::uint(&values, 4).to_string()), false))
}

/// A token's bonding curve offered beside the exchanges for the same trade against QUAI. A token
/// whose market is its curve (still bonding, or bonded onto a pool the curve itself keeps) can
/// have a shallow pool elsewhere; the router alone would send a trade there, or refuse it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CurveOffer {
    pub token: String,
    pub symbol: String,
    pub curve: String,
    /// Selling the token to the curve (for QUAI), not buying it.
    pub sell: bool,
    /// Base units in and out.
    pub input: String,
    pub output: String,
    pub fee: String,
    pub family: crate::capabilities::Family,
}

impl CurveOffer {
    /// The output as a number, for comparing with an exchange's.
    pub fn amount(&self) -> Option<U256> {
        U256::from_str_radix(&self.output, 10).ok()
    }
}

/// Quote trading `amount` of `from` for `to` on `token`'s curve, when one side is QUAI and the other
/// that token. `Ok(None)` for any other pair.
pub async fn curve_offer(
    ctx: &crate::data::DataCtx,
    token: &crate::markets::PoolToken,
    curve: &str,
    from: &crate::swap::SwapAsset,
    to: &crate::swap::SwapAsset,
    amount: U256,
) -> Result<Option<CurveOffer>> {
    use crate::swap::SwapAsset;
    let is_token = |a: &SwapAsset| matches!(a, SwapAsset::Token { address, .. } if address.eq_ignore_ascii_case(&token.address));
    let sell = match (from, to) {
        (SwapAsset::Quai, t) if is_token(t) => false,
        (f, SwapAsset::Quai) if is_token(f) => true,
        _ => return Ok(None),
    };
    let (output, fee, _, hartii) = quote_on_curve(ctx, addr(READ_CALLER)?, &token.address, curve, amount, sell).await?;
    Ok(Some(CurveOffer {
        token: token.address.to_lowercase(),
        symbol: token.symbol.clone(),
        curve: curve.to_lowercase(),
        sell,
        input: amount.to_string(),
        output: output.to_string(),
        fee: fee.to_string(),
        family: if hartii { crate::capabilities::Family::HartiiCurve } else { crate::capabilities::Family::QuainanceCurve },
    }))
}

/// `amount × (1 − slippage)`, rounded down.
fn minimum(amount: U256, slippage_bps: u16) -> U256 {
    crate::swap::minimum_out(amount, slippage_bps)
}

impl Session {
    /// Public preview, including `max` for the selected account. No reservation or signing.
    pub async fn curve_trade_quote(
        &self,
        account: Option<&str>,
        token: &str,
        curve: &str,
        value: &str,
        sell: bool,
        slippage: u16,
    ) -> Result<CurveTradeQuote> {
        crate::swap::validate_slippage(slippage)?;
        let ctx = self.data_ctx_at(crate::data::Trust::FirstHand)?;
        // A quote reads; only `max` needs an account to read a balance from. A watch-only wallet
        // quotes from the read-only caller, as a swap quote does.
        let owner = match self.account(account) {
            Ok(a) => a.address,
            Err(_) if !value.eq_ignore_ascii_case("max") => READ_CALLER.to_string(),
            Err(e) => return Err(e),
        };
        let caller = addr(&owner)?;
        let metadata = crate::markets::token_meta_required(&ctx, token).await?;
        let input = if value.eq_ignore_ascii_case("max") {
            if sell {
                Erc20::new(addr(token)?, &ctx.node.provider)?.balance_of(caller, caller, BlockTag::Latest).await?
            } else {
                let (balance, gas) = tokio::try_join!(
                    ctx.node.provider.balance(caller, BlockTag::Latest),
                    ctx.node.provider.gas_price(crate::network::ZONE)
                )?;
                crate::spendable::quai_max(balance, gas, 500_000).amount
            }
        } else {
            crate::amount::parse_amount(value, if sell { metadata.decimals } else { 18 })?
        };
        crate::swap::require_minimum(input)?;
        let (output, fee, excess, hartii) = quote_on_curve(&ctx, caller, token, curve, input, sell).await?;
        let minimum = crate::swap::minimum_out(output, slippage);
        crate::swap::require_minimum(minimum)?;
        Ok(CurveTradeQuote {
            family: if hartii { crate::capabilities::Family::HartiiCurve } else { crate::capabilities::Family::QuainanceCurve },
            token: metadata,
            curve: curve.into(),
            owner,
            sell,
            input: input.to_string(),
            expected_output: output.to_string(),
            minimum_output: minimum.to_string(),
            maximum_fee: fee.to_string(),
            excess_credit: excess,
            immediate_native_payment: hartii,
            onchain_deadline: !hartii,
            observed_at: crate::registry::now(),
        })
    }
    async fn curve_for(&self, token: &str, curve: &str) -> Result<(QuaiAddress, Contract<'_, crate::network::WalletTransport>)> {
        let address = verified_curve(&self.app, &self.node, &self.network, token, curve, crate::data::Trust::FirstHand).await?;
        let contract = Contract::new(address, interface(CURVE_ABI)?, &self.node.provider);
        let graduated = contract.call(addr(READ_CALLER)?, "graduated", &[], BlockTag::Latest).await?;
        if graduated.first().and_then(|v| v.as_bool()).unwrap_or(false) {
            return Err(CoreError::Rejected("this token has graduated from its curve and trades in its pair now".into()));
        }
        Ok((address, contract))
    }

    fn curve_trust(&self) -> &'static str {
        self.network.ecosystem.curve_launcher.as_ref().map_or("", |p| p.trust_label_on(&self.node))
    }

    /// Review buying on a curve with `quai` QUAI. The curve quotes it first; the review names the
    /// fee, the price paid against the curve's current price, and any overshoot credited back.
    #[allow(clippy::too_many_arguments)]
    pub async fn review_curve_buy(
        &mut self,
        account: Option<&str>,
        token: &str,
        _symbol: &str,
        curve: &str,
        quai: &str,
        slippage_bps: u16,
        deadline_minutes: u32,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        if crate::hartii_tx::matches_curve_runtime(&self.data_ctx_at(crate::data::Trust::FirstHand)?, curve).await? {
            return self.review_hartii_buy(account, token, curve, quai, slippage_bps, max_fee).await;
        }
        crate::swap::validate_slippage(slippage_bps)?;
        crate::swap::validate_deadline(deadline_minutes)?;
        let gross = crate::amount::parse_quai(quai)?;
        if gross.is_zero() {
            return Err(CoreError::Invalid("amount must be greater than zero".into()));
        }
        let from = self.account(account)?;
        let owner = addr(&from.address)?;
        let (address, contract) = self.curve_for(token, curve).await?;
        let metadata = crate::markets::token_meta_required(&self.data_ctx_at(crate::data::Trust::FirstHand)?, token).await?;
        let (symbol, decimals) = (metadata.symbol.as_str(), metadata.decimals);
        let balance = self.node.provider.balance(owner, BlockTag::Latest).await?;
        if balance < gross {
            return Err(CoreError::Insufficient(format!("QUAI balance is {}", crate::amount::quai(balance))));
        }
        let caller = addr(READ_CALLER)?;
        let quote = contract.call(caller, "quoteBuy", &[json!(gross.to_string())], BlockTag::Latest).await?;
        let n = |i: usize| crate::chain::uint(&quote, i);
        let (used, fee, tokens, excess, graduates) = (n(0), n(2), n(3), n(4), quote.get(5).and_then(|v| v.as_bool()).unwrap_or(false));
        if tokens.is_zero() {
            return Err(CoreError::Rejected("the curve would give nothing for this amount".into()));
        }
        let min_tokens = minimum(tokens, slippage_bps);
        crate::swap::require_minimum(min_tokens)?;
        let deadline = crate::registry::now() + u64::from(deadline_minutes) * 60;
        let call = contract.prepare("buy", &[json!(min_tokens.to_string()), json!(deadline.to_string())], gross)?;
        let call = crate::data::with_access_list(&self.node.provider, owner, call).await?;
        let paid = crate::amount::to_f64(used, 18) / crate::amount::to_f64(tokens, decimals);
        let mut warnings =
            vec!["a new token's price is set by its curve alone: early buyers, including its creator, can sell into you".into()];
        if graduates {
            warnings.push(
                "this buy completes the curve: the token graduates into its pair, and any QUAI past the target is credited to claim".into(),
            );
        }
        let short = |v: U256| crate::amount::group_thousands(&crate::amount::format_amount_short(v, decimals, 4));
        let mut fields = vec![
            field("Token", format!("{symbol} ({token})")),
            field("You pay", format!("{} QUAI", crate::amount::quai(gross))),
            field("Expected", format!("≈ {} {symbol}", short(tokens))),
            field("Minimum received", format!("{} {symbol}  (slippage {:.2}%)", short(min_tokens), f64::from(slippage_bps) / 100.0)),
            field("Average price", format!("{paid:.10} QUAI per {symbol}")),
            field("Curve fee", format!("{} QUAI", crate::amount::quai(fee))),
            field("Curve", format!("{address} (named by the Quainance launcher, {})", self.curve_trust())),
            field("Deadline", format!("{deadline_minutes} min")),
        ];
        if !excess.is_zero() {
            fields.push(field("Credited back", format!("{} QUAI past the target, to claim", crate::amount::quai(excess))));
        }
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: "curve_buy".into(),
            title: format!("Buy {symbol} on its bonding curve"),
            asset: "QUAI".into(),
            amount: gross,
            decimals: crate::amount::QUAI_DECIMALS,
            counterparty: address.to_string(),
            fields,
            warnings,
            detail: json!({"expires_at": deadline, "token": token, "curve": address.to_string(), "recipient": owner.to_string(), "to_token": token, "to_symbol": symbol, "to_decimals": decimals, "expected_out": tokens.to_string(), "financial_effects": [
                {"direction":"out","asset":"QUAI","token":"quai","decimals":18,"amount":gross.to_string(),"estimated":false,"note":"maximum payment including fees and any credit retained on the curve"},
                {"direction":"in","asset":symbol,"token":token,"decimals":decimals,"amount":tokens.to_string(),"minimum":min_tokens.to_string(),"estimated":true,"note":"tokens delivered to signer"},
                {"direction":"in","asset":"QUAI curve credit","token":"quai","decimals":18,"amount":excess.to_string(),"estimated":true,"note":"requires a separate claim"}
            ]}),
            max_gas: 400_000,
            max_fee: self.parse_fee_cap(max_fee, crate::amount::QUAI_DECIMALS)?,
        })
        .await
    }

    /// Review selling `amount` tokens back to the curve. Needs an exact approval first (the
    /// `ApprovalNeeded` error says so), and credits the QUAI to claim rather than sending it.
    #[allow(clippy::too_many_arguments)]
    pub async fn review_curve_sell(
        &mut self,
        account: Option<&str>,
        token: &str,
        _symbol: &str,
        curve: &str,
        amount: &str,
        slippage_bps: u16,
        deadline_minutes: u32,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        if crate::hartii_tx::matches_curve_runtime(&self.data_ctx_at(crate::data::Trust::FirstHand)?, curve).await? {
            return self.review_hartii_sell(account, token, curve, amount, slippage_bps, max_fee).await;
        }
        crate::swap::validate_slippage(slippage_bps)?;
        crate::swap::validate_deadline(deadline_minutes)?;
        let from = self.account(account)?;
        let owner = addr(&from.address)?;
        let (address, contract) = self.curve_for(token, curve).await?;
        let metadata = crate::markets::token_meta_required(&self.data_ctx_at(crate::data::Trust::FirstHand)?, token).await?;
        let (symbol, decimals) = (metadata.symbol.as_str(), metadata.decimals);
        let atoms = crate::amount::parse_amount(amount, decimals)?;
        crate::swap::require_minimum(atoms)?;
        let erc = Erc20::new(addr(token)?, &self.node.provider)?;
        let balance = erc.balance_of(owner, owner, BlockTag::Latest).await?;
        if balance < atoms {
            return Err(CoreError::Insufficient(format!("{symbol} balance is {}", crate::amount::format_amount(balance, decimals))));
        }
        if erc.allowance(owner, owner, address, BlockTag::Latest).await? < atoms {
            return Err(approval_needed(
                token,
                format!("approve exactly {} {symbol} for its curve first (step 1 of 2)", crate::amount::format_amount(atoms, decimals)),
            ));
        }
        let quote = contract.call(addr(READ_CALLER)?, "quoteSell", &[json!(atoms.to_string())], BlockTag::Latest).await?;
        let n = |i: usize| crate::chain::uint(&quote, i);
        let (used, fee, credit) = (n(0), n(2), n(3));
        if credit.is_zero() {
            return Err(CoreError::Rejected("the curve would pay nothing for this amount".into()));
        }
        let min_credit = minimum(credit, slippage_bps);
        crate::swap::require_minimum(min_credit)?;
        let deadline = crate::registry::now() + u64::from(deadline_minutes) * 60;
        let call = contract.prepare(
            "sell",
            &[json!(atoms.to_string()), json!(min_credit.to_string()), json!(deadline.to_string())],
            U256::ZERO,
        )?;
        let call = crate::data::with_access_list(&self.node.provider, owner, call).await?;
        let mut warnings = vec!["the QUAI is credited to you on the curve, not sent: claim it afterwards (c on Launches)".into()];
        if used < atoms {
            warnings.push(format!("the curve takes only {} of these tokens", crate::amount::format_amount(used, decimals)));
        }
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: "curve_sell".into(),
            title: format!("Sell {symbol} to its bonding curve"),
            asset: symbol.to_string(),
            amount: atoms,
            decimals,
            counterparty: address.to_string(),
            fields: vec![
                field("Token", format!("{symbol} ({token})")),
                field("You sell", format!("{} {symbol}", crate::amount::format_amount(atoms, decimals))),
                field("Credited", format!("≈ {} QUAI", crate::amount::quai(credit))),
                field(
                    "Minimum credited",
                    format!("{} QUAI  (slippage {:.2}%)", crate::amount::quai(min_credit), f64::from(slippage_bps) / 100.0),
                ),
                field("Curve fee", format!("{} QUAI", crate::amount::quai(fee))),
                field("Curve", format!("{address} (named by the Quainance launcher, {})", self.curve_trust())),
                field("Deadline", format!("{deadline_minutes} min")),
            ],
            warnings,
            detail: json!({"expires_at": deadline, "token": token, "curve": address.to_string(), "decimals": decimals, "recipient":owner.to_string(), "financial_effects":[
                {"direction":"out","asset":symbol,"token":token,"decimals":decimals,"amount":atoms.to_string(),"estimated":false,"note":"maximum tokens sold; unused tokens remain"},
                {"direction":"in","asset":"QUAI curve credit","token":"quai","decimals":18,"amount":credit.to_string(),"minimum":min_credit.to_string(),"estimated":true,"note":"requires a separate claim"}
            ]}),
            max_gas: 300_000,
            max_fee: self.parse_fee_cap(max_fee, crate::amount::QUAI_DECIMALS)?,
        })
        .await
    }

    /// Review the exact approval a curve sale needs (step 1 of 2).
    pub async fn review_curve_sell_approval(
        &mut self,
        account: Option<&str>,
        token: &str,
        _symbol: &str,
        curve: &str,
        amount: &str,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        if crate::hartii_tx::matches_curve_runtime(&self.data_ctx_at(crate::data::Trust::FirstHand)?, curve).await? {
            return self.review_hartii_sell_approval(account, token, curve, amount, max_fee).await;
        }
        let from = self.account(account)?;
        let owner = addr(&from.address)?;
        let (address, _) = self.curve_for(token, curve).await?;
        let metadata = crate::markets::token_meta_required(&self.data_ctx_at(crate::data::Trust::FirstHand)?, token).await?;
        let (symbol, decimals) = (metadata.symbol.as_str(), metadata.decimals);
        let atoms = crate::amount::parse_amount(amount, decimals)?;
        crate::swap::require_minimum(atoms)?;
        let atoms = self.bounded_allowance(token, owner, address, atoms).await?;
        let call = Erc20::new(addr(token)?, &self.node.provider)?.approve(address, atoms)?;
        let call = crate::data::with_access_list(&self.node.provider, owner, call).await?;
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: "approve".into(),
            title: format!("Approve {symbol} for its curve (step 1 of 2)"),
            asset: symbol.to_string(),
            amount: atoms,
            decimals,
            counterparty: address.to_string(),
            fields: vec![
                field("Token contract", token.to_string()),
                field("Spender", format!("{address} (its bonding curve, named by the Quainance launcher, {})", self.curve_trust())),
                field("Allowance", format!("exactly {} {symbol}", crate::amount::format_amount(atoms, decimals))),
            ],
            warnings: vec![],
            detail: json!({"token": token, "purpose": "curve_sell", "spender": address.to_string(), "decimals": decimals}),
            max_gas: 120_000,
            max_fee: self.parse_fee_cap(max_fee, crate::amount::QUAI_DECIMALS)?,
        })
        .await
    }

    /// Review claiming the QUAI a curve holds for this account (sales, and overshoot past the target).
    pub async fn review_curve_claim(
        &mut self,
        account: Option<&str>,
        token: &str,
        symbol: &str,
        curve: &str,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        if crate::hartii_tx::matches_curve_runtime(&self.data_ctx_at(crate::data::Trust::FirstHand)?, curve).await? {
            return Err(CoreError::Rejected("Hartii pays native QUAI directly; it has no separate claim action".into()));
        }
        let from = self.account(account)?;
        let owner = addr(&from.address)?;
        // Claiming works after graduation too, so only the launcher's word is checked here.
        let address = verified_curve(&self.app, &self.node, &self.network, token, curve, crate::data::Trust::FirstHand).await?;
        let contract = Contract::new(address, interface(CURVE_ABI)?, &self.node.provider);
        let owed = contract.call(addr(READ_CALLER)?, "claimableQuote", &[json!(from.address)], BlockTag::Latest).await?;
        let owed = crate::chain::uint(&owed, 0);
        if owed.is_zero() {
            return Err(CoreError::NotFound(format!("nothing to claim on {symbol}'s curve")));
        }
        let call = contract.prepare("claimQuote", &[json!(from.address)], U256::ZERO)?;
        let call = crate::data::with_access_list(&self.node.provider, owner, call).await?;
        self.prepare_account(AccountRequest {
            from: from.clone(),
            intent: call.into_account_intent(),
            kind: "curve_claim".into(),
            title: format!("Claim QUAI from {symbol}'s curve"),
            asset: "QUAI".into(),
            amount: owed,
            decimals: crate::amount::QUAI_DECIMALS,
            counterparty: address.to_string(),
            fields: vec![
                field("Claiming", format!("{} QUAI", crate::amount::quai(owed))),
                field("To", from.address.clone()),
                field("Curve", format!("{address} (named by the Quainance launcher, {})", self.curve_trust())),
            ],
            warnings: vec![],
            detail: json!({"token": token, "curve": address.to_string(), "recipient":owner.to_string(), "financial_effects":[
                {"direction":"in","asset":"QUAI","token":"quai","decimals":18,"amount":owed.to_string(),"estimated":true,"note":"claim of current curve credit"}
            ]}),
            max_gas: 120_000,
            max_fee: self.parse_fee_cap(max_fee, crate::amount::QUAI_DECIMALS)?,
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The drawn curve is the marginal price between samples, rising along a bonding curve, and
    /// a flat or empty stretch is skipped rather than divided by zero.
    #[test]
    fn the_price_curve_is_quai_raised_over_tokens_sold() {
        // NOAH's curve sampled at 0, 12,500 and 25,000 QUAI raised (2026-09-17).
        let points = price_points(25_000.0, &[0.0, 592_105_263.0, 750_000_000.0]);
        assert_eq!(points.len(), 2);
        assert_eq!(points[0].0, 6_250.0, "each price sits at its step's midpoint");
        assert!(points[1].1 > points[0].1, "the price rises along the curve: {points:?}");
        assert!((points[0].1 - 12_500.0 / 592_105_263.0).abs() < 1e-12);
        assert!(price_points(25_000.0, &[5.0, 5.0]).is_empty(), "a flat step has no price");
        assert!(price_points(25_000.0, &[]).is_empty());
    }

    /// `price_points` describes the Quainance-family curve, which has an explicit graduation
    /// target in QUAI and is sampled with `cumulativeTokensSold`. Given no target there is no
    /// x-axis to sample, so it must come back empty rather than dividing by a zero-width step.
    /// Hartii curves do not use this at all: they are constant-product virtual-reserve curves and
    /// build their points from their own reserves.
    #[test]
    fn a_curve_with_no_target_has_no_axis_to_draw_against() {
        let flat = vec![0.0; CURVE_POINTS + 1];
        assert!(price_points(0.0, &flat).is_empty(), "no target means no drawable curve");
        assert!(price_points(0.0, &[0.0, 1_000.0]).iter().all(|p| p.1 == 0.0), "a zero target prices every step at zero");
    }

    #[test]
    fn minimums_round_down_and_slippage_is_capped() {
        assert_eq!(minimum(U256::from(10_000u64), 50), U256::from(9_950u64));
        assert_eq!(minimum(U256::from(10_000u64), 5_000), U256::from(5_000u64), "validated tolerance may give up at most half");
    }
}
