//! Authenticated Hartii curve adapter. Its quote methods exclude the fee; buy takes native
//! QUAI and sell pays native QUAI directly to the signer. Neither method takes a deadline.
use crate::chain::{addr, address_at, interface, uint};
use crate::data::{DataCtx, READ_CALLER, Trust};
use crate::error::{CoreError, Result, approval_needed};
use crate::markets::PoolToken;
use crate::session::Session;
use crate::tx::{AccountRequest, Review, field};
use quai_sdk::contracts::{Contract, Erc20};
use quai_sdk::{BlockTag, QuaiAddress, U256};
use serde_json::json;

#[derive(Clone, Debug)]
pub struct VerifiedCurve {
    pub address: QuaiAddress,
    pub token: PoolToken,
    pub fee_bps: u16,
    pub graduated: bool,
}

/// Where the Hartii launcher keeps `curveOf(token)`: a mapping at slot 12. Read against
/// `curveOf()` at the same block on mainnet (2026-09-24, QMOON and PEPEQUAI); the live test
/// `the_hartii_launchpad_reads_its_curves` reads it again.
pub const CURVE_OF_SLOT: u64 = 12;

/// The runtime of an EIP-1167 clone of `implementation`.
fn clone_code(implementation: &str) -> Option<Vec<u8>> {
    hex::decode(format!("363d3d373d3d3d363d73{}5af43d82803e903d91602b57fd5bf3", implementation.trim_start_matches("0x"))).ok()
}

fn clone_matches(code: &[u8], implementation: &str) -> bool {
    clone_code(implementation).is_some_and(|expected| code == expected)
}

/// On a review, the launcher's own record of the token's curve and the code of both clones,
/// proven at one block (confirmed by the network's RPC when a monitor serves the reads). The
/// calls beside it are the node's word; this is what a buy's destination rests on.
async fn prove_curve(
    ctx: &DataCtx,
    launcher: QuaiAddress,
    token: &str,
    curve: QuaiAddress,
    curve_impl: &str,
    token_impl: &str,
) -> Result<()> {
    let token = addr(token)?;
    let slot = crate::anchor::mapping_field_slot(token, CURVE_OF_SLOT, 0);
    let targets: [(QuaiAddress, &[quai_sdk::primitives::Hash32]); 3] = [(launcher, &[slot]), (curve, &[]), (token, &[])];
    let Some((proven, _)) = crate::anchor::prove_state(&ctx.node, &ctx.network, &targets, "Hartii curve").await? else {
        return Ok(());
    };
    let hash = |implementation: &str| {
        clone_code(implementation).map(|code| quai_sdk::primitives::Hash32::from_bytes(quai_sdk::crypto::keccak256(&code)))
    };
    let named = proven[0].storage_value(slot).map(crate::anchor::word_address).unwrap_or_default();
    if !named.eq_ignore_ascii_case(&curve.to_string()) {
        return Err(CoreError::Rejected(
            "the Hartii launcher's own records do not name this curve for the token; refusing to use it".into(),
        ));
    }
    if hash(curve_impl) != Some(proven[1].code_hash()) {
        return Err(CoreError::Rejected("Hartii curve is not the exact proxy of its pinned implementation".into()));
    }
    if hash(token_impl) != Some(proven[2].code_hash()) {
        return Err(CoreError::Rejected("Hartii token is not the exact proxy of the launcher's pinned token implementation".into()));
    }
    Ok(())
}

/// Preliminary adapter detection from exact proxy bytes. This grants no trust by itself;
/// every read/review subsequently runs the full launcher, implementation and token verifier.
pub async fn matches_curve_runtime(ctx: &DataCtx, curve: &str) -> Result<bool> {
    let Some(implementation) = ctx.network.ecosystem.hartii_curve_impl.as_ref() else { return Ok(false) };
    let code = ctx.node.provider.code(addr(curve)?, BlockTag::Latest).await?;
    Ok(clone_matches(code.bytes(), &implementation.address))
}

/// Verify launcher, implementation, exact EIP-1167 proxy and both reverse identities. Required
/// token units come directly from the contract; discovery names never size a transaction.
pub async fn verified_curve(ctx: &DataCtx, token: &str, curve: &str) -> Result<VerifiedCurve> {
    let eco = &ctx.network.ecosystem;
    let launcher_pin = eco.hartii_launcher.as_ref().ok_or_else(|| CoreError::NotFound("no Hartii launcher on this network".into()))?;
    let impl_pin =
        eco.hartii_curve_impl.as_ref().ok_or_else(|| CoreError::NotFound("no Hartii curve implementation on this network".into()))?;
    let token_pin =
        eco.hartii_token_impl.as_ref().ok_or_else(|| CoreError::NotFound("no Hartii token implementation pin on this network".into()))?;
    let caller = addr(READ_CALLER)?;
    let address = addr(curve)?;
    let launcher = addr(&launcher_pin.address)?;
    let factory = Contract::new(launcher, interface(crate::hartii::LAUNCHER_ABI)?, &ctx.node.provider);
    let contract = Contract::new(address, interface(crate::hartii::CURVE_ABI)?, &ctx.node.provider);
    let token_arg = [json!(token)];
    // Every read below is independent of the others, so they go out as one round rather than
    // fifteen in a row: on a review the pins are checked afresh, and in sequence this was seconds
    // before a buy could even be quoted. Nothing is trusted until every check after it has passed.
    let (verified, named, named_impl, named_token_impl, token_code, code, actual_token, actual_factory, fee, graduated, meta) = tokio::try_join!(
        async {
            tokio::try_join!(
                ctx.verify_pinned(token_pin, "Hartii token implementation"),
                ctx.verify_pinned(launcher_pin, "Hartii launcher"),
                ctx.verify_pinned(impl_pin, "Hartii curve implementation"),
            )
        },
        async { Ok::<_, CoreError>(factory.call(caller, "curveOf", &token_arg, BlockTag::Latest).await?) },
        async { Ok::<_, CoreError>(factory.call(caller, "curveImplementation", &[], BlockTag::Latest).await?) },
        async { Ok::<_, CoreError>(factory.call(caller, "tokenImplementation", &[], BlockTag::Latest).await?) },
        async { Ok::<_, CoreError>(ctx.node.provider.code(addr(token)?, BlockTag::Latest).await?) },
        async { Ok::<_, CoreError>(ctx.node.provider.code(address, BlockTag::Latest).await?) },
        async { Ok::<_, CoreError>(contract.call(caller, "token", &[], BlockTag::Latest).await?) },
        async { Ok::<_, CoreError>(contract.call(caller, "factory", &[], BlockTag::Latest).await?) },
        async { Ok::<_, CoreError>(contract.call(caller, "feeBps", &[], BlockTag::Latest).await?) },
        async { Ok::<_, CoreError>(contract.call(caller, "graduated", &[], BlockTag::Latest).await?) },
        crate::markets::token_meta_required(ctx, token),
    )?;
    let (token_impl, launcher_verified, implementation) = verified;
    debug_assert_eq!(launcher, launcher_verified);
    if !address_at(&named, 0).eq_ignore_ascii_case(curve) || !address_at(&named_impl, 0).eq_ignore_ascii_case(&implementation.to_string()) {
        return Err(CoreError::Rejected("Hartii launcher does not authenticate this token, curve and implementation".into()));
    }
    if !address_at(&named_token_impl, 0).eq_ignore_ascii_case(&token_impl.to_string())
        || !clone_matches(token_code.bytes(), &token_impl.to_string())
    {
        return Err(CoreError::Rejected("Hartii token is not the exact proxy of the launcher's pinned token implementation".into()));
    }
    if !clone_matches(code.bytes(), &implementation.to_string()) {
        return Err(CoreError::Rejected("Hartii curve is not the exact proxy of its pinned implementation".into()));
    }
    if !address_at(&actual_token, 0).eq_ignore_ascii_case(token)
        || !address_at(&actual_factory, 0).eq_ignore_ascii_case(&launcher.to_string())
    {
        return Err(CoreError::Rejected("Hartii curve does not name its token and launcher back".into()));
    }
    if !ctx.trust.may_cache() {
        prove_curve(ctx, launcher, token, address, &implementation.to_string(), &token_impl.to_string()).await?;
    }
    let fee_bps = u16::try_from(uint(&fee, 0)).map_err(|_| CoreError::Invalid("Hartii fee is unreadable".into()))?;
    crate::hartii::after_fee(U256::from(1), fee_bps)?;
    let graduated =
        graduated.first().and_then(|v| v.as_bool()).ok_or_else(|| CoreError::Invalid("Hartii graduation state is unreadable".into()))?;
    let token = meta;
    Ok(VerifiedCurve { address, token, fee_bps, graduated })
}

/// Focused Hartii market data. `spot_price` is the reserve ratio once the reserves reproduce the
/// curve's own one-QUAI quote, else that quote inverted (fee included); zero means the quote crosses
/// graduation and has an unpriced native refund.
///
/// Hartii has no graduation-target field and no `cumulativeTokensSold` sampler, so a
/// Quainance-shaped read of one yields nothing to draw. Its curve is a constant product over
/// virtual reserves, so the target and the chart are derived from those instead — but only once
/// the reserve model has reproduced the contract's own `quoteBuy`. A curve whose model is not
/// confirmed keeps `target == 0` and empty `points` rather than showing a shape nobody verified.
pub async fn market(ctx: &DataCtx, token: &str, curve: &str, owners: &[String]) -> Result<crate::curve::CurveMarket> {
    let target = verified_curve(ctx, token, curve).await?;
    let caller = addr(READ_CALLER)?;
    let contract = Contract::new(target.address, interface(crate::hartii::CURVE_ABI)?, &ctx.node.provider);
    let gross = crate::amount::parse_quai("1")?;
    let (net, _) = crate::hartii::after_fee(gross, target.fee_bps)?;
    // One round of reads, the quote with them: nothing here depends on another answer.
    let quote_arg = [json!(net.to_string())];
    let (supply, sold, raised, virtual_quai, virtual_token, pool_quai, pool_token, quoted) = tokio::try_join!(
        contract.call(caller, "curveSupply", &[], BlockTag::Latest),
        contract.call(caller, "tokensSold", &[], BlockTag::Latest),
        contract.call(caller, "realQuaiReserve", &[], BlockTag::Latest),
        contract.call(caller, "virtualQuaiReserve", &[], BlockTag::Latest),
        contract.call(caller, "virtualTokenReserve", &[], BlockTag::Latest),
        contract.call(caller, "poolQuaiReserve", &[], BlockTag::Latest),
        contract.call(caller, "poolTokenReserve", &[], BlockTag::Latest),
        contract.call(caller, "quoteBuy", &quote_arg, BlockTag::Latest),
    )?;
    let (supply, sold, raised) = (uint(&supply, 0), uint(&sold, 0), uint(&raised, 0));
    let (virtual_quai, virtual_token) = (uint(&virtual_quai, 0), uint(&virtual_token, 0));
    let (pool_quai, pool_token, quoted) = (uint(&pool_quai, 0), uint(&pool_token, 0), uint(&quoted, 0));
    let output = bounded_buy_output(quoted, supply, sold, target.graduated)?;
    let decimals = target.token.decimals;
    // Which reserves the curve trades against is decided by the contract, not by the field names:
    // whichever reading reproduces the `quoteBuy` just read is the live one.
    let current =
        crate::hartii::live_reserves(target.graduated, virtual_quai, virtual_token, raised, sold, pool_quai, pool_token, net, quoted);
    let spot_price = if output.is_zero() || output < quoted {
        0.0
    } else {
        current
            .and_then(|(q, t)| crate::hartii::reserve_spot(q, t, decimals))
            .unwrap_or_else(|| 1.0 / crate::amount::to_f64(output, decimals))
    };
    // Only a curve still selling has a sell-out to draw toward; a bonded one's pool is past it.
    let (curve_target, points) = current
        .filter(|_| !target.graduated)
        .and_then(|(q, t)| crate::hartii::launch_reserves(q, t, raised, sold))
        .and_then(|(q0, t0)| {
            let sellout = crate::hartii::raise_at_sellout(q0, t0, supply)?;
            let samples = crate::hartii::sellout_samples(q0, t0, sellout, crate::curve::CURVE_POINTS, decimals)?;
            Some((sellout, crate::curve::price_points(crate::amount::to_f64(sellout, crate::amount::QUAI_DECIMALS), &samples)))
        })
        .unwrap_or((U256::ZERO, Vec::new()));
    let erc = Erc20::new(addr(token)?, &ctx.node.provider)?;
    let mut held = U256::ZERO;
    for owner in owners {
        held = held
            .checked_add(erc.balance_of(caller, addr(owner)?, BlockTag::Latest).await?)
            .ok_or_else(|| CoreError::Invalid("Hartii aggregate balance overflows".into()))?;
    }
    Ok(crate::curve::CurveMarket {
        token_decimals: target.token.decimals,
        token: token.to_lowercase(),
        curve: curve.to_lowercase(),
        curve_tokens: supply,
        tokens_sold: sold,
        raised,
        target: curve_target,
        fee_bps: u64::from(target.fee_bps),
        spot_price,
        progress_bps: crate::hartii::progress_bps(raised, curve_target, sold, supply),
        graduated: target.graduated,
        points,
        held,
        claimable: U256::ZERO,
    })
}

fn warnings() -> Vec<String> {
    vec![
        "Hartii curve trades have no on-chain deadline; the minimum output remains the execution bound".into(),
        "a launch token can move sharply as its creator and other holders sell".into(),
    ]
}

/// Buy output is capped only before bonding. The exact runtime refunds unused native QUAI
/// immediately when a buy fills this allocation; it does not create a claimable credit.
fn bounded_buy_output(raw: U256, supply: U256, sold: U256, graduated: bool) -> Result<U256> {
    if graduated {
        return Ok(raw);
    }
    let remaining = supply.checked_sub(sold).ok_or_else(|| CoreError::Invalid("Hartii sold allocation exceeds its supply".into()))?;
    Ok(raw.min(remaining))
}

impl Session {
    pub async fn review_hartii_buy(
        &mut self,
        account: Option<&str>,
        token: &str,
        curve: &str,
        value: &str,
        slippage_bps: u16,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        crate::swap::validate_slippage(slippage_bps)?;
        let gross = crate::amount::parse_quai(value)?;
        crate::swap::require_minimum(gross)?;
        let from = self.account(account)?;
        let owner = addr(&from.address)?;
        let target = verified_curve(&self.data_ctx_at(Trust::FirstHand)?, token, curve).await?;
        let (net, fee) = crate::hartii::after_fee(gross, target.fee_bps)?;
        let contract = Contract::new(target.address, interface(crate::hartii::CURVE_ABI)?, &self.node.provider);
        let net_arg = [json!(net.to_string())];
        let (raw, supply, sold, balance) = tokio::try_join!(
            async { Ok::<_, CoreError>(uint(&contract.call(owner, "quoteBuy", &net_arg, BlockTag::Latest).await?, 0)) },
            async { Ok::<_, CoreError>(uint(&contract.call(owner, "curveSupply", &[], BlockTag::Latest).await?, 0)) },
            async { Ok::<_, CoreError>(uint(&contract.call(owner, "tokensSold", &[], BlockTag::Latest).await?, 0)) },
            async { Ok::<_, CoreError>(self.node.provider.balance(owner, BlockTag::Latest).await?) },
        )?;
        let expected = bounded_buy_output(raw, supply, sold, target.graduated)?;
        let refunds_excess = !target.graduated && raw >= supply.saturating_sub(sold);
        let mut warning = warnings();
        if refunds_excess {
            warning.push("this buy fills the curve allocation; unused QUAI is refunded directly in this transaction".into());
        }
        let minimum = crate::swap::minimum_out(expected, slippage_bps);
        crate::swap::require_minimum(minimum)?;
        if balance < gross {
            return Err(CoreError::Insufficient("not enough QUAI for this Hartii purchase".into()));
        }
        let call = contract.prepare("buy", &[json!(minimum.to_string())], gross)?;
        let call = crate::data::with_access_list(&self.node.provider, owner, call).await?;
        let symbol = &target.token.symbol;
        self.prepare_account(AccountRequest {
            from, intent: call.into_account_intent(), kind: "hartii_buy".into(), title: format!("Buy {symbol} on Hartii"),
            asset: "QUAI".into(), amount: gross, decimals: crate::amount::QUAI_DECIMALS, counterparty: curve.into(),
            fields: vec![field("Token", format!("{symbol} ({token})")), field(if refunds_excess { "Maximum payment" } else { "You pay" }, format!("{} QUAI", crate::amount::quai(gross))),
                field("Expected", format!("{} {symbol}", crate::amount::format_amount(expected,target.token.decimals))),
                field("Minimum received", format!("{} {symbol}", crate::amount::format_amount(minimum,target.token.decimals))),
                field(if refunds_excess { "Maximum curve fee" } else { "Curve fee" }, format!("{} QUAI", crate::amount::quai(fee))), field("Curve", curve.to_string()),
                field("Recipient", owner.to_string()), field("Deadline", "not provided by this contract")],
            warnings: warning,
            detail: json!({"recipient":owner.to_string(),"to_token":token,"token":token,"curve":curve,"to_symbol":symbol,"to_decimals":target.token.decimals,"expected_out":expected.to_string(),"minimum_out":minimum.to_string(),"refunds_excess":refunds_excess,
                "financial_effects":[{"direction":"out","asset":"QUAI","token":"quai","decimals":18,"amount":gross.to_string(),"estimated":refunds_excess,"note":if refunds_excess { "maximum native payment; excess refunded directly" } else { "native curve payment" }},
                {"direction":"in","asset":symbol,"token":token,"decimals":target.token.decimals,"amount":expected.to_string(),"minimum":minimum.to_string(),"estimated":true,"note":"protected curve output"}]}),
            max_gas: 500_000, max_fee:self.parse_fee_cap(max_fee,crate::amount::QUAI_DECIMALS)?,
        }).await
    }

    pub async fn review_hartii_sell_approval(
        &mut self,
        account: Option<&str>,
        token: &str,
        curve: &str,
        value: &str,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        let from = self.account(account)?;
        let owner = addr(&from.address)?;
        let target = verified_curve(&self.data_ctx_at(Trust::FirstHand)?, token, curve).await?;
        let amount = crate::amount::parse_amount(value, target.token.decimals)?;
        crate::swap::require_minimum(amount)?;
        let amount = self.bounded_allowance(token, owner, target.address, amount).await?;
        let call = Erc20::new(addr(token)?, &self.node.provider)?.approve(target.address, amount)?;
        let call = crate::data::with_access_list(&self.node.provider, owner, call).await?;
        let symbol = &target.token.symbol;
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: "approve".into(),
            title: format!("Approve {symbol} for Hartii"),
            asset: symbol.clone(),
            amount,
            decimals: target.token.decimals,
            counterparty: curve.into(),
            fields: vec![
                field("Token contract", token),
                field("Spender", format!("{curve} (authenticated Hartii curve)")),
                field("Allowance", format!("exactly {} {symbol}", crate::amount::format_amount(amount, target.token.decimals))),
            ],
            warnings: warnings(),
            detail: json!({"token":token,"spender":curve,"purpose":"hartii_sell","decimals":target.token.decimals}),
            max_gas: 120_000,
            max_fee: self.parse_fee_cap(max_fee, crate::amount::QUAI_DECIMALS)?,
        })
        .await
    }

    pub async fn review_hartii_sell(
        &mut self,
        account: Option<&str>,
        token: &str,
        curve: &str,
        value: &str,
        slippage_bps: u16,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        crate::swap::validate_slippage(slippage_bps)?;
        let from = self.account(account)?;
        let owner = addr(&from.address)?;
        let target = verified_curve(&self.data_ctx_at(Trust::FirstHand)?, token, curve).await?;
        let amount = crate::amount::parse_amount(value, target.token.decimals)?;
        crate::swap::require_minimum(amount)?;
        let erc = Erc20::new(addr(token)?, &self.node.provider)?;
        let contract = Contract::new(target.address, interface(crate::hartii::CURVE_ABI)?, &self.node.provider);
        let amount_arg = [json!(amount.to_string())];
        let (balance, allowance, gross) = tokio::try_join!(
            async { Ok::<_, CoreError>(erc.balance_of(owner, owner, BlockTag::Latest).await?) },
            async { Ok::<_, CoreError>(erc.allowance(owner, owner, target.address, BlockTag::Latest).await?) },
            // Its failure is reported after the balance and allowance checks, as it was when it ran last.
            async { Ok::<_, CoreError>(contract.call(owner, "quoteSell", &amount_arg, BlockTag::Latest).await) },
        )?;
        if balance < amount {
            return Err(CoreError::Insufficient("not enough tokens for this Hartii sale".into()));
        }
        if allowance < amount {
            return Err(approval_needed(token, "approve the exact token amount for this Hartii curve first"));
        }
        let gross = uint(&gross?, 0);
        let (expected, fee) = crate::hartii::after_fee(gross, target.fee_bps)?;
        let minimum = crate::swap::minimum_out(expected, slippage_bps);
        crate::swap::require_minimum(minimum)?;
        let call = contract.prepare("sell", &[json!(amount.to_string()), json!(minimum.to_string())], U256::ZERO)?;
        let call = crate::data::with_access_list(&self.node.provider, owner, call).await?;
        let symbol = &target.token.symbol;
        self.prepare_account(AccountRequest{
            from,intent:call.into_account_intent(),kind:"hartii_sell".into(),title:format!("Sell {symbol} on Hartii"),asset:symbol.clone(),amount,decimals:target.token.decimals,counterparty:curve.into(),
            fields:vec![field("Token",format!("{symbol} ({token})")),field("You sell",format!("{} {symbol}",crate::amount::format_amount(amount,target.token.decimals))),field("Expected",format!("{} QUAI",crate::amount::quai(expected))),field("Minimum received",format!("{} QUAI",crate::amount::quai(minimum))),field("Curve fee",format!("{} QUAI",crate::amount::quai(fee))),field("Recipient",owner.to_string()),field("Deadline","not provided by this contract")],
            warnings:warnings(),detail:json!({"recipient":owner.to_string(),"to_token":"quai","token":token,"curve":curve,"decimals":target.token.decimals,
                "financial_effects":[{"direction":"out","asset":symbol,"token":token,"decimals":target.token.decimals,"amount":amount.to_string(),"estimated":false,"note":"curve sale"},
                {"direction":"in","asset":"QUAI","token":"quai","decimals":18,"amount":expected.to_string(),"minimum":minimum.to_string(),"estimated":true,"note":"native proceeds sent directly to signer"}]}),
            max_gas:500_000,max_fee:self.parse_fee_cap(max_fee,crate::amount::QUAI_DECIMALS)?,
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bonding_output_caps_at_allocation_but_bonded_trading_continues() {
        let (raw, supply, sold) = (U256::from(200), U256::from(1000), U256::from(900));
        assert_eq!(bounded_buy_output(raw, supply, sold, false).unwrap(), U256::from(100));
        assert_eq!(bounded_buy_output(raw, supply, sold, true).unwrap(), raw);
        assert!(bounded_buy_output(raw, sold, supply, false).is_err());
    }

    #[test]
    fn hartii_fee_is_removed_before_buy_quote_and_after_sell_quote() {
        let gross = crate::amount::parse_quai("1").unwrap();
        let (net, fee) = crate::hartii::after_fee(gross, 100).unwrap();
        assert_eq!(net, crate::amount::parse_quai("0.99").unwrap());
        assert_eq!(fee, crate::amount::parse_quai("0.01").unwrap());
        assert_eq!(crate::hartii::after_fee(U256::from(1), 100).unwrap(), (U256::from(1), U256::ZERO));
        assert_eq!(crate::hartii::after_fee(U256::MAX, 10_000).unwrap(), (U256::ZERO, U256::MAX));
        assert!(crate::hartii::after_fee(gross, 10_001).is_err());
    }

    #[test]
    fn only_exact_clone_to_pinned_implementation_is_authenticated() {
        let implementation = "0x0062d75a096e67fef48a8c5a9fd9094d2bea9d14";
        let code = hex::decode(format!("363d3d373d3d3d363d73{}5af43d82803e903d91602b57fd5bf3", &implementation[2..])).unwrap();
        assert!(clone_matches(&code, implementation));
        let mut changed = code.clone();
        changed[20] ^= 1;
        assert!(!clone_matches(&changed, implementation));
        let mut suffixed = code;
        suffixed.push(0);
        assert!(!clone_matches(&suffixed, implementation));
    }
}
