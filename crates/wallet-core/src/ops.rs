//! High-level wallet operations shared by the CLI and TUI. Every value-moving
//! operation returns a [`Review`]; callers then `commit` or `discard` it.

use crate::amount::{self, QI_DECIMALS, QUAI_DECIMALS};
use crate::appdb::Token;
use crate::data::Trust;
use crate::error::{CoreError, Result};
use crate::network::ZONE;
use crate::registry::{QuaiAccount, parse_any_address};
use crate::session::{Session, new_operation_id, qi_stale, stale_pause};
use crate::tx::{AccountRequest, Field, Pending, Review, field};
use quai_sdk::accounts::AccountIntent;
use quai_sdk::consensus::{ConversionSlippage, Denomination, QiConversionIntent, QiWrappingIntent};
use quai_sdk::contracts::Erc20;
use quai_sdk::payment_channels::{
    ChannelRegistration, MailboxDiscovery, MailboxRegistration, PaymentScanOptions, discover_mailbox_channels, payment_intent,
    scan_payment_channel,
};
use quai_sdk::payment_mailbox::PaymentMailbox;
use quai_sdk::payments::{PaymentChannel, PaymentCode};
use quai_sdk::provider::{QiFeeProfile, RpcData};
use quai_sdk::qi::{QiChangePool, QiError, QiIntent, QiPolicy, QiSession, QiSpecialIntent};
use quai_sdk::wallet::{AggregationPolicy, SweepMode};
use quai_sdk::wrappers::{QiRedemptionPlan, WrappedQi, WrappedQuai, qits_to_wqi_atoms};
use quai_sdk::{BlockTag, Ledger, QiAddress, QuaiAddress, U256};
use serde::{Deserialize, Serialize};

/// Explicit no-change sweep preview, in exact base units.
#[derive(Clone, Debug, Serialize)]
pub struct QiSweepQuote {
    /// Amount delivered after the exact estimated fee, in qits.
    pub amount_qits: String,
    /// Estimated shape-dependent fee in qits.
    pub fee_qits: String,
    /// Number of selected eligible inputs.
    pub inputs: usize,
    /// Ordered recipient address and denomination value, in qits.
    pub outputs: Vec<(String, u64)>,
}

/// Current-fee, exact-qit MAX fill. This is an advisory observation, never a signable review.
#[derive(Clone, Debug, Serialize)]
pub struct QiSpecialMax {
    pub amount: String,
    pub amount_qits: String,
    pub fee_qits: String,
    pub inputs: usize,
    pub outputs: usize,
    pub excluded_inputs: usize,
    pub excluded_qits: String,
    pub candidate_height: String,
    pub whole_qi: bool,
}

fn parse_sweep_destinations(destinations: &[String]) -> Result<Vec<QiAddress>> {
    if destinations.is_empty() || destinations.len() > 256 {
        return Err(CoreError::Invalid("sweep needs 1–256 distinct fresh Qi destination addresses, one per output".into()));
    }
    let mut seen = std::collections::HashSet::new();
    destinations
        .iter()
        .map(|text| {
            let address: QiAddress =
                parse_any_address(text)?.try_into().map_err(|_| CoreError::Invalid("a sweep destination must be a Qi address".into()))?;
            if !seen.insert(address) {
                return Err(CoreError::Invalid("each sweep output needs a different fresh destination".into()));
            }
            Ok(address)
        })
        .collect()
}

/// Unlimited ERC-20 approval amount.
pub const UNLIMITED: U256 = U256::MAX;

/// A parsed send destination.
#[derive(Clone, Debug)]
pub enum Recipient {
    /// Quai ledger address.
    Quai(QuaiAddress),
    /// Qi ledger address.
    Qi(QiAddress),
    /// BIP47 payment code.
    PaymentCode(PaymentCode),
}

/// Token balance row.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenBalance {
    /// Token.
    pub token: Token,
    /// Owner.
    pub owner: String,
    /// Balance (atoms).
    #[serde(serialize_with = "crate::ser::u256", deserialize_with = "crate::ser::de_u256")]
    pub balance: U256,
}

/// Payment-channel peer view.
#[derive(Clone, Debug, Serialize)]
pub struct PeerView {
    /// Peer payment code.
    pub code: String,
    /// Contact name, when known.
    pub contact: Option<String>,
    /// Receive addresses discovered for this peer.
    pub receive_addresses: usize,
    /// Send addresses allocated to this peer.
    pub send_addresses: usize,
}

/// One message in a sealed conversation, as the wallet shows it.
#[derive(Clone, Debug, Serialize)]
pub struct SealedLine {
    /// Unix seconds.
    pub at: u64,
    /// Sender address.
    pub from: String,
    /// Sent by this wallet.
    pub mine: bool,
    /// The text, or `None` when the body will not open for this conversation.
    pub text: Option<String>,
    /// Posted from an account not recorded for this contact. A sealed body opening is not proof
    /// of who posted it (anyone can copy a body onto the board from their own address), so this
    /// is shown, never recorded; naming the account is the user's call.
    pub new_address: bool,
}

/// Conversion quote with settlement-risk scenarios.
#[derive(Clone, Debug, Serialize)]
pub struct ConversionQuote {
    /// `quai_to_qi` or `qi_to_quai`.
    pub direction: String,
    /// Input amount (source base units).
    pub amount: String,
    /// Human input.
    pub amount_display: String,
    /// Node quote for this amount alone (destination base units), when available.
    pub quoted: Option<String>,
    /// Human quote.
    pub quoted_display: Option<String>,
    /// Controller-discounted estimate of what the conversion pays (`quai_calculateConversionAmount`),
    /// destination base units. This, not the spot quote, is what users should expect.
    pub expected: Option<String>,
    /// Human expected amount.
    pub expected_display: Option<String>,
    /// What this conversion loses right now, alone: the gap between the node's undiscounted rate
    /// and its own estimate, in basis points. A **lower bound** on what the batch will cost, and
    /// never a threshold to refuse on — it carries a rate-basis gap of tens of bps in either
    /// direction, because the rate is read against the zone header and the estimate against the
    /// prime terminus.
    pub implied_slippage_bps: Option<u16>,
    /// The discount has reached the node's ten-percent floor, where no slippage setting changes
    /// the outcome and the conversion simply pays a tenth.
    pub discount_saturated: bool,
    /// Set while the node refuses conversions at this height.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold: Option<ConversionHold>,
    /// One plain sentence: what goes in, what comes out, and what the discount costs.
    pub headline: String,
    /// Block conversion flow amount (Its), when the header reports it.
    pub flow_amount: Option<String>,
    /// Discount scenarios.
    pub scenarios: Vec<RiskScenario>,
    /// Suggested slippage (bps) that clears the "you plus one similar competitor" scenario.
    pub suggested_slippage_bps: u16,
    /// Minimum allowed input, when protocol-bound.
    pub minimum: Option<String>,
    /// Notes shown with the quote.
    pub notes: Vec<String>,
    /// The explorer's step-by-step preview (explanation only; mainnet, market data on).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explorer_steps: Option<crate::explorer::ConversionSteps>,
}

/// Margin added over the larger of the batch model and the node's own estimate when suggesting a
/// slippage tolerance. Half a percent: enough to absorb the rate-basis gap between the two figures
/// and a block of ordinary flow, without quietly offering to pay much more than the quote implies.
pub const SLIPPAGE_MARGIN_BPS: u16 = 50;

/// Where the node's cubic discount stops growing because its ten-percent floor has taken over.
/// `ConversionSlippage` caps at the same number, so at or above it a tolerance is not a choice.
pub const SATURATED_BPS: u16 = 9000;

/// The node refuses conversions in two hard-coded windows after each k-Quai controller change.
#[derive(Clone, Debug, Serialize)]
pub struct ConversionHold {
    /// Prime height at which the window closes and the same conversion becomes acceptable.
    pub until_prime: u64,
    /// Prime blocks still to go.
    pub blocks_remaining: u64,
    /// True when going ahead destroys value: a Quai→Qi conversion is refused inside the EVM, so it
    /// is mined and its nonce and gas are burned. Qi→QUAI is refused at pool admission and costs
    /// nothing, so it is only worth a warning.
    pub burns_gas: bool,
    /// Ready-made explanation for any surface.
    pub note: String,
}

/// Whether this direction is inside one of the pinned node's hold windows at `prime_terminus`.
///
/// Mainnet is past both and cannot re-enter one; Orchard is still below the second, which opens at
/// [`SHA_EQUIVALENT_DIFFICULTY_FORK_BLOCK`] — the same height that makes the fee profile apply, so
/// conversions there become priceable and unacceptable in the same block.
///
/// [`SHA_EQUIVALENT_DIFFICULTY_FORK_BLOCK`]: quai_sdk::consensus::SHA_EQUIVALENT_DIFFICULTY_FORK_BLOCK
pub fn conversion_hold(direction: &str, prime_terminus: u64) -> Option<ConversionHold> {
    use quai_sdk::consensus::{KAWPOW_FORK_BLOCK, KQUAI_CHANGE_HOLD_INTERVAL, SHA_EQUIVALENT_DIFFICULTY_FORK_BLOCK, conversion_held};
    if !conversion_held(prime_terminus) {
        return None;
    }
    let until_prime = [KAWPOW_FORK_BLOCK, SHA_EQUIVALENT_DIFFICULTY_FORK_BLOCK]
        .into_iter()
        .map(|fork| fork + KQUAI_CHANGE_HOLD_INTERVAL)
        .find(|end| prime_terminus < *end)?;
    let burns_gas = direction == "quai_to_qi";
    let blocks_remaining = until_prime.saturating_sub(prime_terminus);
    let note = if burns_gas {
        format!(
            "This chain is holding conversions until prime block {until_prime} ({blocks_remaining} to go). A QUAI→Qi conversion is refused inside the EVM, so it is still mined and its gas is burned for nothing. Wrapping is not held."
        )
    } else {
        format!(
            "This chain is holding conversions until prime block {until_prime} ({blocks_remaining} to go). A Qi→QUAI conversion is refused when it is submitted, so nothing is spent and the same transaction works once the window passes. Wrapping is not held."
        )
    };
    Some(ConversionHold { until_prime, blocks_remaining, burns_gas, note })
}

/// The one sentence every surface leads with: in, out, and the cost of the discount.
///
/// The zero case is not "free". The node's cubic discount is at least 20 bps for any amount, but
/// the rate and the estimate are read against different blocks — the zone header and the prime
/// terminus — and at small sizes that basis gap is larger than the discount and can even make the
/// estimate come out above the rate. Saying "no discount" there would be a promise the protocol
/// does not make, so the sentence says the discount is too small to see instead.
fn conversion_headline(amount: &str, expected: Option<&str>, quoted: Option<&str>, implied_bps: Option<u16>) -> String {
    match (expected, implied_bps) {
        (Some(out), Some(bps)) if bps > 0 => {
            format!("{amount} → about {out}, which is {} below the rate right now", percent(bps))
        }
        (Some(out), Some(_)) => format!("{amount} → about {out}; at this size the discount is too small to measure against the rate"),
        (Some(out), None) => format!("{amount} → about {out}"),
        (None, _) => match quoted {
            Some(spot) => format!("{amount} → about {spot} at the rate; the node did not price the discount"),
            None => format!("{amount} → no quote available"),
        },
    }
}

/// Basis points as a percentage a person reads without converting units.
pub fn percent(bps: u16) -> String {
    format!("{}.{:02}%", bps / 100, bps % 100)
}

/// One batch-discount scenario.
#[derive(Clone, Debug, Serialize)]
pub struct RiskScenario {
    /// Description.
    pub label: String,
    /// Batch total in QUAI (human).
    pub batch_quai: String,
    /// Discount in basis points.
    pub discount_bps: u16,
}

/// Wrapped asset balances for an account.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WrapStatus {
    /// Account.
    pub account: String,
    /// WQI token balance (atoms).
    pub wqi_atoms: Option<String>,
    /// WQI balance in Qi.
    pub wqi_qi: Option<String>,
    /// Unclaimed wrapped Qi backing (Qits).
    pub unclaimed_qits: Option<String>,
    /// WQUAI balance (atoms).
    pub wquai_atoms: Option<String>,
}

/// Everything the dashboard's contract reads produce, gathered in one pass.
///
/// `wrap` and `wrap_error` are the two halves of one answer: a wrap card either has numbers or a
/// reason it has none, and the dashboard shows whichever it got.
#[derive(Clone, Debug, Default)]
pub struct DashboardBalances {
    /// Token balances across every account asked for.
    pub tokens: Vec<TokenBalance>,
    /// Wrapped asset balances for the wrap card's account.
    pub wrap: Option<WrapStatus>,
    /// Why there are none (not configured, no account, node error).
    pub wrap_error: Option<String>,
}

/// Number of fixed-denomination outputs needed to pay `amount` exactly (greedy).
pub fn denomination_count(amount: U256) -> usize {
    let mut remaining = u128::try_from(amount).unwrap_or(u128::MAX);
    let mut count = 0usize;
    for value in Denomination::VALUES.iter().rev() {
        let v = u128::from(*value);
        count += (remaining / v) as usize;
        remaining %= v;
    }
    count
}

impl Session {
    fn quai_contract(&self, value: &Option<String>, name: &str) -> Result<QuaiAddress> {
        value
            .as_ref()
            .ok_or_else(|| CoreError::Network(format!("{name} is not configured on {}", self.network.id)))?
            .parse()
            .map_err(|_| CoreError::Invalid(format!("{name} address is invalid")))
    }

    /// The network's WQI (`qi`) or WQUAI contract, with its runtime bytecode checked against the
    /// pinned hash where the network has one (mainnet). Returns the address and its trust label.
    async fn wrapper_contract(&self, qi: bool) -> Result<(QuaiAddress, &'static str)> {
        let (address, hash, name) = if qi {
            (&self.network.wqi, &self.network.ecosystem.wqi_code_hash, "WQI")
        } else {
            (&self.network.wquai, &self.network.ecosystem.wquai_code_hash, "WQUAI")
        };
        self.quai_contract(address, name)?;
        let pin = crate::network::PinnedContract { address: address.clone().unwrap_or_default(), code_hash: hash.clone() };
        // A wrapper address is a call destination in the review that follows: read its code now.
        let contract =
            crate::data::verify_pinned(&self.app, &self.node, &self.network, &pin, &format!("{name} contract"), Trust::FirstHand).await?;
        Ok((contract, pin.trust_label()))
    }

    /// Parse a recipient: contact name, Quai/Qi address or payment code.
    pub fn resolve_recipient(&self, text: &str) -> Result<Recipient> {
        let trimmed = text.trim();
        if let Some(contact) = self.app.contact(trimmed)? {
            if let Some(code) = contact.payment_code {
                return Ok(Recipient::PaymentCode(PaymentCode::from_base58(&code)?));
            }
            if let Some(address) = contact.address {
                return self.resolve_recipient(&address);
            }
        }
        if trimmed.starts_with("0x") {
            let address = parse_any_address(trimmed)?;
            return Ok(match address.ledger() {
                Ledger::Quai => Recipient::Quai(address.try_into().map_err(|_| CoreError::Invalid("bad Quai address".into()))?),
                Ledger::Qi => Recipient::Qi(address.try_into().map_err(|_| CoreError::Invalid("bad Qi address".into()))?),
            });
        }
        PaymentCode::from_base58(trimmed)
            .map(Recipient::PaymentCode)
            .map_err(|_| CoreError::Invalid(format!("`{trimmed}` is not a contact, address or payment code")))
    }

    pub(crate) fn quai_address_of(account: &QuaiAccount) -> Result<QuaiAddress> {
        account.address.parse().map_err(|_| CoreError::Storage(format!("invalid account {}", account.address)))
    }

    pub(crate) fn parse_fee_cap(&self, text: Option<&str>, decimals: u8) -> Result<Option<U256>> {
        text.map(|t| amount::parse_amount(t, decimals)).transpose()
    }

    // ------------------------------------------------------------------ Quai

    /// Review a native QUAI transfer.
    pub async fn review_send_quai(&mut self, from: Option<&str>, to: &str, value: &str, max_fee: Option<&str>) -> Result<Review> {
        let from = self.account(from)?;
        let recipient = match self.resolve_recipient(to)? {
            Recipient::Quai(a) => a,
            Recipient::Qi(_) | Recipient::PaymentCode(_) => {
                return Err(CoreError::Invalid("that is a Qi destination; use `send qi` or convert first".into()));
            }
        };
        let value = amount::parse_quai(value)?;
        if value.is_zero() {
            return Err(CoreError::Invalid("amount must be greater than zero".into()));
        }
        let mut warnings = Vec::new();
        if self.meta.quai_accounts.iter().any(|a| a.address.eq_ignore_ascii_case(&recipient.to_string())) {
            warnings.push("recipient is one of your own accounts".into());
        }
        self.prepare_account(AccountRequest {
            from,
            intent: AccountIntent {
                to: recipient,
                value,
                data: RpcData::new(vec![]).map_err(|_| CoreError::Invalid("calldata".into()))?,
                access_list: vec![],
            },
            kind: "send_quai".into(),
            title: "Send QUAI".into(),
            asset: "QUAI".into(),
            amount: value,
            decimals: QUAI_DECIMALS,
            counterparty: recipient.to_string(),
            fields: vec![],
            warnings,
            detail: serde_json::json!({}),
            max_gas: 100_000,
            max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
        })
        .await
    }

    /// Review a 0 QUAI self-transfer that consumes the lowest unused (released) nonce, unblocking
    /// transactions queued behind it. Needed before conversions, which cannot reuse a nonce.
    pub async fn review_fill_gap(&mut self, from: Option<&str>) -> Result<Review> {
        let from = self.account(from)?;
        let Some((nonce, _)) = self.nonce_gaps(&from).await?.first().copied() else {
            return Err(CoreError::Invalid(format!("account {} has no nonce gap to fill", from.label)));
        };
        let me = Self::quai_address_of(&from)?;
        self.prepare_account(AccountRequest {
            intent: AccountIntent {
                to: me,
                value: U256::ZERO,
                data: RpcData::new(vec![]).map_err(|_| CoreError::Invalid("calldata".into()))?,
                access_list: vec![],
            },
            kind: "fill_gap".into(),
            title: "Fill nonce gap".into(),
            asset: "QUAI".into(),
            amount: U256::ZERO,
            decimals: QUAI_DECIMALS,
            counterparty: from.address.clone(),
            fields: vec![field("Purpose", format!("use nonce {nonce} so later transactions from this account can be mined"))],
            warnings: vec![],
            detail: serde_json::json!({"nonce": nonce}),
            max_gas: 100_000,
            max_fee: None,
            from,
        })
        .await
    }

    // ------------------------------------------------------------------ tokens

    /// Import an ERC-20 token by contract address, reading its metadata.
    pub async fn import_token(&mut self, address: &str) -> Result<Token> {
        let contract: QuaiAddress =
            address.trim().parse().map_err(|_| CoreError::Invalid("token address must be a Cyprus-1 Quai address".into()))?;
        let caller = self.caller()?;
        let erc = Erc20::new(contract, &self.node.provider)?;
        let read_str = |values: Vec<serde_json::Value>| values.first().and_then(|v| v.as_str()).map(sanitize_display);
        let symbol =
            erc.contract().call(caller, "symbol", &[], BlockTag::Latest).await.ok().and_then(read_str).unwrap_or_else(|| "TOKEN".into());
        let name =
            erc.contract().call(caller, "name", &[], BlockTag::Latest).await.ok().and_then(read_str).unwrap_or_else(|| symbol.clone());
        let decimals = erc
            .contract()
            .call(caller, "decimals", &[], BlockTag::Latest)
            .await
            .map_err(|e| CoreError::Network(format!("token did not return decimals (not an ERC-20?): {e}")))?
            .first()
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u8>().ok())
            .ok_or_else(|| CoreError::Invalid("token decimals invalid".into()))?;
        if decimals > 77 {
            return Err(CoreError::Invalid("token decimals out of range".into()));
        }
        let token = Token { network: self.network.id.clone(), address: contract.to_string(), symbol, name, decimals, hidden: false };
        self.app.upsert_token(&token)?;
        self.app.token(&self.network.id, &token.address)
    }

    /// Ensure the default wrapped tokens are listed for this network.
    pub fn ensure_default_tokens(&mut self) -> Result<()> {
        for (addr, symbol, name) in [(self.network.wqi.clone(), "WQI", "Wrapped Qi"), (self.network.wquai.clone(), "WQUAI", "Wrapped Quai")]
        {
            if let Some(addr) = addr
                && self.app.token(&self.network.id, &addr).is_err()
            {
                self.app.upsert_token(&Token {
                    network: self.network.id.clone(),
                    address: addr,
                    symbol: symbol.into(),
                    name: name.into(),
                    decimals: 18,
                    hidden: false,
                })?;
            }
        }
        Ok(())
    }

    fn caller(&self) -> Result<QuaiAddress> {
        match self.meta.quai_accounts.first() {
            Some(a) => Self::quai_address_of(a),
            None => self
                .meta
                .watch
                .iter()
                .find_map(|w| w.address.parse::<QuaiAddress>().ok())
                .ok_or_else(|| CoreError::Invalid("wallet has no Quai address to read contracts with".into())),
        }
    }

    /// Token balances for one account.
    pub async fn token_balances(&mut self, account: Option<&str>) -> Result<Vec<TokenBalance>> {
        self.ensure_default_tokens()?;
        let owner = Self::quai_address_of(&self.account(account)?)?;
        let tokens = self.app.tokens(&self.network.id, false)?;
        // One batched read for every token when the network has Multicall3.
        let reader = &self.node;
        if let Some(mc) = crate::multicall::Multicall::on(&self.app, reader, &self.network, Trust::Cached).await {
            use crate::multicall::{Arg, Call, word};
            let owner_text = owner.to_string();
            let calls: Vec<Call> =
                tokens.iter().map(|t| Call::view(&t.address, "balanceOf(address)", &[Arg::Addr(owner_text.clone())])).collect();
            if let Ok(results) = mc.try_all(&calls).await {
                return Ok(tokens
                    .into_iter()
                    .zip(results)
                    .map(|(token, data)| TokenBalance {
                        balance: data.as_deref().map_or(U256::ZERO, |d| word(d, 0)),
                        token,
                        owner: owner_text.clone(),
                    })
                    .collect());
            }
        }
        let mut out = Vec::new();
        for token in tokens {
            let contract: QuaiAddress = token.address.parse().map_err(|_| CoreError::Storage("bad token address".into()))?;
            let balance = Erc20::new(contract, self.provider())?.balance_of(owner, owner, BlockTag::Latest).await.unwrap_or(U256::ZERO);
            out.push(TokenBalance { token, owner: owner.to_string(), balance });
        }
        Ok(out)
    }

    /// The account whose wrapped balances the wrap card shows: the default one, or the first
    /// watched Quai address when the wallet cannot sign.
    fn wrap_owner(&self) -> Result<QuaiAddress> {
        match self.account(None) {
            Ok(a) => Self::quai_address_of(&a),
            Err(e) => match self.quai_owner_addresses().first() {
                Some(a) => a.parse().map_err(|_| CoreError::Invalid(format!("invalid watched address {a}"))),
                None => Err(e),
            },
        }
    }

    /// Every `balanceOf` the dashboard needs, in one round trip where the network allows it:
    /// each account's token balances and the wrap card's WQI and WQUAI balances.
    ///
    /// The refresh used to walk accounts one at a time and then read the wrap card separately, so
    /// a wallet with five accounts and eight tokens spent dozens of serial calls on what
    /// Multicall3 answers in one. Only the unclaimed-backing read stays on its own, because it is
    /// a node method (`quai_getWrappedQiDeposit`) rather than a contract call.
    pub async fn dashboard_balances(&mut self, accounts: &[String]) -> Result<DashboardBalances> {
        self.ensure_default_tokens()?;
        let tokens = self.app.tokens(&self.network.id, false)?;
        let wrap_owner = self.wrap_owner().ok();
        let wrapped = (self.network.wqi.is_some() || self.network.wquai.is_some()).then_some(wrap_owner).flatten();
        let reader = &self.node;
        let Some(mc) = crate::multicall::Multicall::on(&self.app, reader, &self.network, Trust::Cached).await else {
            return self.dashboard_balances_serially(accounts).await;
        };
        use crate::multicall::{Arg, Call, word};
        let mut calls: Vec<Call> = Vec::with_capacity(accounts.len() * tokens.len() + 2);
        for owner in accounts {
            for token in &tokens {
                calls.push(Call::view(&token.address, "balanceOf(address)", &[Arg::Addr(owner.clone())]));
            }
        }
        // The two wrapped balances ride along at the end, where their offsets are known.
        let wrap_at = calls.len();
        if let (Some(owner), Some(wqi)) = (&wrapped, &self.network.wqi) {
            calls.push(Call::view(wqi, "balanceOf(address)", &[Arg::Addr(owner.to_string())]));
        }
        if let (Some(owner), Some(wquai)) = (&wrapped, &self.network.wquai) {
            calls.push(Call::view(wquai, "balanceOf(address)", &[Arg::Addr(owner.to_string())]));
        }
        let Ok(results) = mc.try_all(&calls).await else {
            return self.dashboard_balances_serially(accounts).await;
        };
        let mut out = DashboardBalances::default();
        for (i, owner) in accounts.iter().enumerate() {
            for (j, token) in tokens.iter().enumerate() {
                let data = results.get(i * tokens.len() + j).and_then(Option::as_ref);
                out.tokens.push(TokenBalance {
                    balance: data.map_or(U256::ZERO, |d| word(d, 0)),
                    token: token.clone(),
                    owner: owner.clone(),
                });
            }
        }
        match wrapped {
            None => out.wrap_error = Some(format!("wrapper contracts are not configured on {}", self.network.id)),
            Some(owner) => {
                let mut next = wrap_at;
                let mut take = || {
                    let data = results.get(next).and_then(Option::as_ref).map(|d| word(d, 0));
                    next += 1;
                    data
                };
                let wqi_atoms = self.network.wqi.is_some().then(&mut take).flatten();
                let wquai_atoms = self.network.wquai.is_some().then(&mut take).flatten();
                // The one read Multicall3 cannot carry: unclaimed backing is a node method.
                let unclaimed = match &self.network.wqi {
                    Some(addr) => match addr.parse().map(|a| WrappedQi::new(a, self.provider())) {
                        Ok(Ok(wqi)) => wqi.unclaimed(owner, BlockTag::Latest).await.ok(),
                        _ => None,
                    },
                    None => None,
                };
                out.wrap = Some(WrapStatus {
                    account: owner.to_string(),
                    wqi_qi: wqi_atoms.map(|a| amount::qi(quai_sdk::wrappers::wqi_atoms_to_qits(a).unwrap_or(U256::ZERO))),
                    wqi_atoms: wqi_atoms.map(|a| a.to_string()),
                    unclaimed_qits: unclaimed.map(|u| u.to_string()),
                    wquai_atoms: wquai_atoms.map(|a| a.to_string()),
                });
            }
        }
        Ok(out)
    }

    /// The same reads without Multicall3: one call at a time, as before.
    async fn dashboard_balances_serially(&mut self, accounts: &[String]) -> Result<DashboardBalances> {
        let mut out = DashboardBalances::default();
        for owner in accounts {
            out.tokens.extend(self.token_balances(Some(owner)).await?);
        }
        match self.wrap_status(None).await {
            Ok(w) => out.wrap = Some(w),
            Err(e) => out.wrap_error = Some(e.to_string()),
        }
        Ok(out)
    }

    /// Review an ERC-20 transfer.
    pub async fn review_send_token(
        &mut self,
        from: Option<&str>,
        token: &str,
        to: &str,
        value: &str,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        self.ensure_default_tokens()?;
        let token = self.app.token(&self.network.id, token)?;
        let from = self.account(from)?;
        let recipient = match self.resolve_recipient(to)? {
            Recipient::Quai(a) => a,
            _ => return Err(CoreError::Invalid("token recipients must be Quai addresses".into())),
        };
        let atoms = amount::parse_amount(value, token.decimals)?;
        let contract: QuaiAddress = token.address.parse().map_err(|_| CoreError::Storage("bad token".into()))?;
        let erc = Erc20::new(contract, &self.node.provider)?;
        let owner = Self::quai_address_of(&from)?;
        let balance = erc.balance_of(owner, owner, BlockTag::Latest).await?;
        if balance < atoms {
            return Err(CoreError::Insufficient(format!("{} balance is {}", token.symbol, amount::format_amount(balance, token.decimals))));
        }
        let call = erc.transfer(recipient, atoms)?;
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: "send_token".into(),
            title: format!("Send {}", token.symbol),
            asset: token.symbol.clone(),
            amount: atoms,
            decimals: token.decimals,
            counterparty: recipient.to_string(),
            fields: vec![field("Token contract", token.address.clone()), field("Call", format!("transfer({recipient}, {atoms})"))],
            warnings: vec![],
            detail: serde_json::json!({"token": token.address, "decimals": token.decimals}),
            max_gas: 200_000,
            max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
        })
        .await
    }

    /// Current allowance.
    pub async fn token_allowance(&mut self, account: Option<&str>, token: &str, spender: &str) -> Result<(Token, U256)> {
        self.ensure_default_tokens()?;
        let token = self.app.token(&self.network.id, token)?;
        let owner = Self::quai_address_of(&self.account(account)?)?;
        let spender: QuaiAddress = spender.trim().parse().map_err(|_| CoreError::Invalid("spender must be a Quai address".into()))?;
        let contract: QuaiAddress = token.address.parse().map_err(|_| CoreError::Storage("bad token".into()))?;
        let value = Erc20::new(contract, &self.node.provider)?.allowance(owner, owner, spender, BlockTag::Latest).await?;
        Ok((token, value))
    }

    /// Review an approval (`amount` None = unlimited; "0" revokes).
    pub async fn review_approve(
        &mut self,
        account: Option<&str>,
        token: &str,
        spender: &str,
        value: Option<&str>,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        self.ensure_default_tokens()?;
        let token = self.app.token(&self.network.id, token)?;
        let from = self.account(account)?;
        let spender: QuaiAddress = spender.trim().parse().map_err(|_| CoreError::Invalid("spender must be a Quai address".into()))?;
        let atoms = match value {
            Some(v) => amount::parse_amount(v, token.decimals)?,
            None => UNLIMITED,
        };
        let contract: QuaiAddress = token.address.parse().map_err(|_| CoreError::Storage("bad token".into()))?;
        let call = Erc20::new(contract, &self.node.provider)?.approve(spender, atoms)?;
        let mut warnings = Vec::new();
        let (kind, title, shown) = if atoms.is_zero() {
            ("revoke", format!("Revoke {} allowance", token.symbol), "0 (revoke)".to_string())
        } else if atoms == UNLIMITED {
            warnings.push(format!("UNLIMITED approval: {spender} will be able to move all of your {} at any time", token.symbol));
            ("approve", format!("Approve {} (unlimited)", token.symbol), "unlimited".to_string())
        } else {
            ("approve", format!("Approve {}", token.symbol), format!("{} {}", amount::format_amount(atoms, token.decimals), token.symbol))
        };
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: kind.into(),
            title,
            asset: token.symbol.clone(),
            amount: if atoms == UNLIMITED { U256::ZERO } else { atoms },
            decimals: token.decimals,
            counterparty: spender.to_string(),
            fields: vec![
                field("Token contract", token.address.clone()),
                field("Spender", spender.to_string()),
                field("Allowance", shown),
            ],
            warnings,
            detail: serde_json::json!({"token": token.address, "decimals": token.decimals, "spender": spender.to_string(), "unlimited": atoms == UNLIMITED}),
            max_gas: 200_000,
            max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
        })
        .await
    }

    // ------------------------------------------------------------------ Qi

    pub(crate) async fn refresh_qi_for_spend(&mut self) -> Result<()> {
        for attempt in 0..5 {
            match quai_sdk::qi_discovery::refresh_qi(&self.node.provider, &mut self.qi_store, 100_000, || false).await {
                Ok(_) => return Ok(()),
                Err(e) if qi_stale(&e) && attempt < 4 => stale_pause().await,
                Err(e) => return Err(e.into()),
            }
        }
        Err(CoreError::Network("Qi state kept changing; try again".into()))
    }

    fn qi_policy(&self, max_fee: U256, max_inputs: usize) -> QiPolicy {
        QiPolicy { initial_fee: U256::ZERO, max_fee, max_inputs, max_outputs: 256, max_fee_rounds: 12, max_snapshot_age: 10 }
    }

    /// Observe the largest verified exact-qit conversion or wrapping fill without consuming
    /// derivation indices or claiming inputs. Preparation later refreshes and reviews real bytes.
    pub async fn quote_qi_special_max(
        &mut self,
        wrapping: bool,
        account: Option<&str>,
        slippage_bps: u16,
        max_fee: Option<&str>,
    ) -> Result<QiSpecialMax> {
        self.require_execution_source()?;
        if !self.network.specialized_fee_estimation {
            return Err(CoreError::Invalid("current-fee Qi MAX requires a qualified specialized estimator on this network".into()));
        }
        let destination = Self::quai_address_of(&self.account(account)?)?;
        if wrapping {
            // Wrapping has a separate account-ledger claim. A Qi-only MAX must
            // not imply the user can complete that step without native fees.
            // This is a current-price budget, not a reservation or future fee guarantee.
            let fee = self
                .node
                .provider
                .gas_price(ZONE)
                .await?
                .checked_mul(U256::from(300_000))
                .ok_or_else(|| CoreError::Invalid("claim gas budget overflow".into()))?;
            let wanted = crate::commitments::Commitments {
                native_value: "0".into(),
                fee: fee.to_string(),
                tokens: Default::default(),
                unknown_assets: false,
            };
            let owner = destination.to_string();
            let _lock = self.commitment_lock(&owner)?;
            if let Err(error) = self.check_commitments(&owner, "", &wanted).await {
                return match error {
                    CoreError::Insufficient(_) => Err(CoreError::Insufficient(format!(
                        "wrapping MAX needs about {} uncommitted QUAI for the later WQI claim at current gas price; fund {owner} or convert some Qi to QUAI first, then refresh MAX",
                        amount::quai(fee)
                    ))),
                    other => Err(other),
                };
            }
        }
        let hd =
            self.meta.qi_account()?.ok_or_else(|| CoreError::Invalid("imported Qi keys use the explicit no-change sweep exit".into()))?;
        let cap = self.parse_fee_cap(max_fee, QI_DECIMALS)?.unwrap_or(U256::from(500));
        let wrapper = if wrapping { Some(self.wrapper_contract(true).await?.0) } else { None };
        self.refresh_qi_for_spend().await?;
        let mut index = self.change_cursor(&hd)?;
        // Derive public capacity only. Neither store metadata nor next derivation index changes.
        let mut preview = Vec::with_capacity(65);
        for _ in 0..65 {
            let found = hd.search(true, quai_sdk::wallet::Search { zone: ZONE, start_index: index, max_attempts: 100_000 }, || false)?;
            index = found.address.index.checked_add(1).ok_or_else(|| CoreError::Invalid("Qi derivation exhausted".into()))?;
            preview.push(quai_sdk::wallet::storage::PublicAddress::derive(&hd, true, found.address.index)?);
        }
        let refund = preview
            .pop()
            .ok_or_else(|| CoreError::Storage("missing preview refund".into()))?
            .address()
            .try_into()
            .map_err(|_| CoreError::Storage("preview refund is not Qi".into()))?;
        let intent = match wrapper {
            Some(owner_contract) => QiSpecialIntent::Wrapping(QiWrappingIntent { destination, owner_contract }),
            None => {
                QiSpecialIntent::Conversion(QiConversionIntent { destination, refund, slippage: ConversionSlippage::new(slippage_bps)? })
            }
        };
        let policy = self.qi_policy(cap, 64);
        let (amount, quote, excluded_inputs, excluded_qits) =
            crate::qi_exit::quote_special_max(&self.node.provider, &mut self.qi_store, intent, policy, &preview).await?;
        Ok(QiSpecialMax {
            amount: amount::qi(amount),
            amount_qits: amount.to_string(),
            fee_qits: quote.fee().to_string(),
            inputs: quote.selected_inputs().len(),
            outputs: quote.transaction().outputs.len(),
            excluded_inputs,
            excluded_qits: excluded_qits.to_string(),
            candidate_height: quote.candidate_height().to_string(),
            whole_qi: false,
        })
    }

    fn change_pool(&mut self, count: usize) -> Result<QiChangePool> {
        let account = self.meta.qi_account()?.ok_or_else(|| CoreError::Invalid("this wallet has no Qi HD account for change".into()))?;
        if count > MAX_CHANGE_POOL {
            return Err(CoreError::Insufficient(format!(
                "this transaction would need about {count} Qi change outputs, more than can be allocated at once (limit {MAX_CHANGE_POOL}); \
                 spend an amount that leaves less change, or consolidate smaller coins first"
            )));
        }
        let attempts = search_budget(count);
        // The SDK bounds a pool to 100,000 derivation attempts in total, so each address
        // gets `attempts`. Cyprus-1 Qi matches are ~1/500 candidates and a deterministic
        // gap wider than the budget would fail every retry at the same index. Look ahead
        // with public derivation and step past any such gap by allocating that single
        // address with a large budget (it is burned as unused change) before the pool.
        for _ in 0..512 {
            let start = self.change_cursor(&account)?;
            let mut index = start;
            let mut blocked = None;
            for _ in 0..count {
                let found =
                    account.search(true, quai_sdk::wallet::Search { zone: ZONE, start_index: index, max_attempts: 100_000 }, || false)?;
                if found.attempts > attempts {
                    blocked = Some(found.address.index);
                    break;
                }
                index = found.address.index + 1;
            }
            trace(format!("change_pool: start {start} count {count} attempts {attempts} blocked {blocked:?}"));
            match blocked {
                None => return Ok(QiChangePool::allocate(&mut self.qi_store, &account, count, attempts, || false)?),
                Some(blocked_index) => {
                    // Consume addresses through the one behind the wide gap so the pool starts after it.
                    while self.change_cursor(&account)? <= blocked_index {
                        self.qi_store.allocate_address_compact(&account, true, 100_000, || false)?;
                    }
                }
            }
        }
        Err(CoreError::Storage("could not allocate Qi change addresses".into()))
    }

    /// Give a pool's unused addresses back, so the next allocation hands them out again instead
    /// of deriving fresh ones. Change that never reached a signed payload is safe to reuse, and
    /// reusing it keeps later change inside the window a seed-only restore scans. A failure here
    /// only means an address stays burned, so it is traced rather than raised.
    fn release_pool(&mut self, pool: QiChangePool) {
        if let Err(e) = pool.release(&mut self.qi_store) {
            trace(format!("change pool not released: {e}"));
        }
    }

    /// An empty pool, to reclaim the change of a prepared transaction that is being thrown away.
    pub(crate) fn empty_change_pool(&mut self) -> Result<QiChangePool> {
        let account = self.meta.qi_account()?.ok_or_else(|| CoreError::Invalid("this wallet has no Qi HD account for change".into()))?;
        Ok(QiChangePool::allocate(&mut self.qi_store, &account, 0, 1, || false)?)
    }

    /// Estimate change outputs for paying `target` from the current snapshot, so the
    /// change pool is small (unused change addresses widen seed-only recovery gaps).
    fn estimate_change(&mut self, target: U256, max_fee: U256) -> Result<usize> {
        let snapshot = self.qi_store.snapshot()?;
        let Some(checkpoint) = snapshot.checkpoint else {
            return Ok(16);
        };
        let height = checkpoint.height.saturating_add(U256::from(1u64));
        let mut most = 0usize;
        for fee in [U256::ZERO, max_fee / U256::from(2u64), max_fee] {
            let request = quai_sdk::wallet::SelectionRequest {
                zone: ZONE,
                candidate_height: height,
                target,
                fee,
                max_fee,
                max_inputs: 64,
                max_outputs: 256,
            };
            match quai_sdk::wallet::select_fewest(&snapshot.coins, &request) {
                Ok(selection) => most = most.max(selection.change_outputs.len()),
                Err(e) => return Err(CoreError::Insufficient(format!("Qi selection: {e}"))),
            }
        }
        trace(format!("estimate_change: {most} change output(s)"));
        Ok((most + 2).clamp(2, 256))
    }

    /// Next raw change index the store will examine.
    fn change_cursor(&self, account: &quai_sdk::wallet::AccountPublic) -> Result<u32> {
        if let Some(next) = self.qi_store.next_derivation_index(account, true)? {
            return Ok(next);
        }
        Ok(self
            .qi_store
            .addresses()?
            .iter()
            .filter_map(|a| match a.origin() {
                quai_sdk::wallet::storage::KeyOrigin::Bip44 { change: true, index, coin: quai_sdk::wallet::CoinType::Qi, .. } => {
                    Some(index + 1)
                }
                _ => None,
            })
            .max()
            .unwrap_or(0))
    }

    fn ensure_channel(&mut self, peer: &PaymentCode) -> Result<()> {
        let payment = self
            .unlocked
            .as_ref()
            .ok_or_else(|| CoreError::Locked("unlock the wallet to use payment codes".into()))?
            .payment
            .as_ref()
            .ok_or_else(|| CoreError::Invalid("this wallet has no payment code".into()))?;
        if payment.public_code() == peer {
            return Err(CoreError::Invalid("that is your own payment code".into()));
        }
        if self.qi_store.payment_channel(payment, peer)?.is_none() {
            self.qi_store.import_payment_channel(payment, &PaymentChannel::new(payment, peer.clone()), None)?;
        }
        let code = peer.to_base58();
        self.app.set_kv(&format!("peer:{}:{}", self.network.id, code), "1")?;
        // A channel now, whatever the mailbox said about it: no longer an offer, nor declined.
        self.app.delete_kv(&offer_key(&self.network.id, &code))?;
        self.app.delete_kv(&declined_key(&self.network.id, &code))?;
        Ok(())
    }

    /// Sweep eligible Qi to explicit external destinations without requiring an HD change root.
    /// Supply one fresh same-zone address per denomination output; no address/key is fabricated.
    /// This is a reviewed exit for imported-key wallets as well as an explicit whole-wallet send.
    pub async fn review_sweep_qi(&mut self, destinations: &[String], max_fee: Option<&str>) -> Result<Review> {
        self.require_execution_source()?;
        self.keys()?;
        let destinations = parse_sweep_destinations(destinations)?;
        let cap = self.parse_fee_cap(max_fee, QI_DECIMALS)?.unwrap_or(U256::from(500u64));
        self.refresh_qi_for_spend().await?;
        let id = new_operation_id()?;
        let _operation_guard = self.operation_lock(id)?;
        let policy = self.qi_policy(cap, 64);
        let prepared = crate::qi_exit::prepare_sweep(&self.node.provider, &mut self.qi_store, id, destinations, policy).await?;
        let amount = prepared.transaction().outputs.iter().fold(U256::ZERO, |sum, output| sum + U256::from(output.denomination.value()));
        let recipients: Vec<_> = prepared.transaction().outputs.iter().map(|output| output.address.to_string()).collect();
        let to = recipients.first().cloned().unwrap_or_default();
        let mut op = self.new_op(
            id,
            "send_qi",
            "qi",
            "qi",
            "QI",
            amount,
            &to,
            serde_json::json!({"mode": "external_sweep", "recipients": recipients, "no_change": true}),
        );
        op.fee = prepared.fee().to_string();
        let reviewed = (|| {
            let warnings = recipients.iter().flat_map(|to| self.recipient_warnings(to)).collect();
            let review = self.qi_review(
                op.clone(),
                "Sweep Qi to external destinations",
                to,
                prepared.transaction(),
                prepared.recipient_outputs(),
                prepared.fee(),
                vec![
                    field("Mode", "all eligible inputs, preserved denominations; no generated change"),
                    field("Received", format!("{} Qi across {} explicit destinations", amount::qi(amount), recipients.len())),
                    field("Fee source", "node estimate for this exact output shape"),
                ],
                warnings,
                prepared.signing_digest().to_string(),
            )?;
            op.detail["review"] = serde_json::to_value(&review)?;
            op.detail["review_version"] = serde_json::json!(1);
            self.journal(op.clone())?;
            Ok::<_, CoreError>(review)
        })();
        let review = match reviewed {
            Ok(review) => review,
            Err(error) => {
                self.qi_store.release_unsigned(id)?;
                return Err(error);
            }
        };
        self.pending.insert(review.op_id.clone(), Pending::QiPortable { prepared, op });
        Ok(review)
    }

    /// Exact read-only sweep amount/fee preview. It neither claims inputs nor allocates change.
    pub async fn quote_sweep_qi(&mut self, destinations: &[String], max_fee: Option<&str>) -> Result<QiSweepQuote> {
        self.require_execution_source()?;
        let destinations = parse_sweep_destinations(destinations)?;
        let cap = self.parse_fee_cap(max_fee, QI_DECIMALS)?.unwrap_or(U256::from(500u64));
        self.refresh_qi_for_spend().await?;
        let policy = self.qi_policy(cap, 64);
        let quote = crate::qi_exit::quote_sweep(&self.node.provider, &mut self.qi_store, destinations, policy).await?;
        let amount = quote.transaction().outputs.iter().fold(U256::ZERO, |sum, output| sum + U256::from(output.denomination.value()));
        Ok(QiSweepQuote {
            amount_qits: amount.to_string(),
            fee_qits: quote.fee().to_string(),
            inputs: quote.transaction().inputs.len(),
            outputs: quote.transaction().outputs.iter().map(|output| (output.address.to_string(), output.denomination.value())).collect(),
        })
    }

    /// Review a Qi payment to a payment code, contact or Qi address.
    pub async fn review_send_qi(&mut self, to: &str, value: &str, max_fee: Option<&str>) -> Result<Review> {
        self.require_execution_source()?;
        let recipient = self.resolve_recipient(to)?;
        let qits = amount::parse_qi(value)?;
        if qits.is_zero() {
            return Err(CoreError::Invalid("amount must be greater than zero".into()));
        }
        let max_fee = self.parse_fee_cap(max_fee, QI_DECIMALS)?.unwrap_or(U256::from(500u64));
        let id = new_operation_id()?;
        let _operation_guard = self.operation_lock(id)?;
        let needed = denomination_count(qits);
        let (mut intent, shown_to, peer) = match &recipient {
            Recipient::PaymentCode(code) => {
                self.ensure_channel(code)?;
                let payment = self.unlocked.as_ref().and_then(|u| u.payment.as_ref()).ok_or_else(no_payment)?;
                let destinations = allocate_payment_destinations(&mut self.qi_store, payment, code, qits, needed.max(1))?;
                (QiIntent { amount: qits, destinations }, code.to_base58(), Some(code.clone()))
            }
            Recipient::Qi(address) => (QiIntent { amount: qits, destinations: vec![*address] }, address.to_string(), None),
            Recipient::Quai(_) => {
                return Err(CoreError::Invalid("that is a Quai address; use `send quai` or `convert qi-to-quai`".into()));
            }
        };
        self.refresh_qi_for_spend().await?;
        let mut pool_size = self.estimate_change(qits, max_fee)?;
        let mut stale = 0;
        // One pool for every attempt. A pool allocated per attempt and then dropped burned its
        // addresses, and enough of those push later change past the gap a seed-only restore
        // scans. Whatever this one does not use goes back at the end.
        let mut pool = Some(self.change_pool(pool_size)?);
        trace(format!("send_qi: change pool of {pool_size}"));
        let prepared = loop {
            trace("send_qi: refreshing Qi");
            self.refresh_qi_for_spend().await?;
            trace("send_qi: building keyring");
            let keys = self
                .unlocked
                .as_ref()
                .ok_or_else(|| CoreError::Locked("wallet is locked".into()))?
                .qi_keyring_with_channels(&self.qi_store)?;
            trace("send_qi: preparing");
            let policy =
                QiPolicy { initial_fee: U256::ZERO, max_fee, max_inputs: 64, max_outputs: 256, max_fee_rounds: 12, max_snapshot_age: 10 };
            let result = QiSession::with_keys(&self.node.provider, &keys, &mut self.qi_store)
                .prepare(id, intent.clone(), policy, pool.as_mut().ok_or_else(|| CoreError::Storage("no change pool".into()))?)
                .await;
            drop(keys);
            trace(format!("send_qi: prepare result ok={}", result.is_ok()));
            match result {
                Ok(p) => break Ok(p),
                // Another writer (the daemon, the Qi lane) refreshed between our refresh and the
                // reservation: refresh again and redo it.
                Err(e) if qi_stale(&e) && stale < 4 => {
                    stale += 1;
                    stale_pause().await;
                }
                Err(QiError::InsufficientChange) if pool_size < 256 => {
                    pool_size = (pool_size * 2).min(256);
                    // Hand the small pool back before asking for a bigger one: an allocation takes
                    // released addresses first, so these come back rather than being burned.
                    if let Some(small) = pool.take() {
                        self.release_pool(small);
                    }
                    pool = Some(self.change_pool(pool_size)?);
                }
                Err(QiError::Selection(quai_sdk::wallet::SelectionError::InsufficientFunds)) => {
                    let spendable = self.qi_summary().map(|q| q.balance.spendable).unwrap_or_default();
                    break Err(CoreError::Insufficient(format!(
                        "{} Qi plus its fee isn't covered by the {} Qi spendable; each small coin spent adds fee, so send less or consolidate first",
                        amount::qi(qits),
                        amount::qi(spendable)
                    )));
                }
                Err(QiError::InsufficientDestinations) => match &peer {
                    Some(code) if intent.destinations.len() < 64 => {
                        let payment = self.unlocked.as_ref().and_then(|u| u.payment.as_ref()).ok_or_else(no_payment)?;
                        let extra = intent.destinations.len();
                        let more = allocate_payment_destinations(&mut self.qi_store, payment, code, qits, extra)?;
                        intent.destinations.extend(more);
                    }
                    Some(_) => break Err(QiError::InsufficientDestinations.into()),
                    None => {
                        break Err(CoreError::Invalid(format!(
                            "{} Qi needs several denomination outputs, and each output needs its own address; send to a payment code instead",
                            amount::qi(qits)
                        )));
                    }
                },
                Err(e) => break Err(e.into()),
            }
        };
        // Whatever was not used goes back, whether or not the prepare worked.
        if let Some(rest) = pool.take() {
            self.release_pool(rest);
        }
        let prepared = prepared?;
        let mut fields = vec![field("Recipient", shown_to.clone())];
        // A raw Qi address gets the same lookalike and dust checks as a Quai one. A payment code
        // pays fresh addresses nobody else can derive, so there is nothing to compare it with.
        let mut warnings = if peer.is_none() { self.recipient_warnings(&shown_to) } else { Vec::new() };
        let mut detail = serde_json::json!({});
        if let Some(code) = &peer {
            fields.push(field("Paid to", format!("{} one-time payment-code addresses", prepared.recipient_outputs())));
            let notified = self.peer_notified(code).await.unwrap_or(None);
            match notified {
                Some(true) => fields.push(field("Mailbox", "recipient already notified")),
                Some(false) => {
                    warnings.push("the recipient has not been notified of this payment code; run `payment notify` (a separate Quai transaction) so Pelagus wallets can find the funds".into());
                    detail = serde_json::json!({"needs_notify": true});
                }
                None => {}
            }
            detail["peer"] = serde_json::json!(code.to_base58());
        }
        let mut op = self.new_op(id, "send_qi", "qi", "qi", "QI", qits, &shown_to, detail);
        let digest = prepared.signing_digest().to_string();
        let review = self.qi_review(
            op.clone(),
            "Send Qi",
            shown_to,
            prepared.transaction(),
            prepared.recipient_outputs(),
            prepared.fee(),
            fields,
            warnings,
            digest,
        )?;
        op.fee = prepared.fee().to_string();
        op.detail["review"] = serde_json::to_value(&review)?;
        op.detail["review_version"] = serde_json::json!(1);
        if let Err(error) = self.journal(op.clone()) {
            self.qi_store.release_unsigned(id)?;
            return Err(error);
        }
        self.pending.insert(review.op_id.clone(), Pending::Qi { prepared, op });
        Ok(review)
    }

    /// Review a consolidation: preserve denominations (sweep) or aggregate small coins.
    pub async fn review_consolidate(&mut self, aggregate: bool, max_fee: Option<&str>) -> Result<Review> {
        self.require_execution_source()?;
        let max_fee = self.parse_fee_cap(max_fee, QI_DECIMALS)?.unwrap_or(U256::from(1000u64));
        let id = new_operation_id()?;
        let _operation_guard = self.operation_lock(id)?;
        let mode = if aggregate { SweepMode::AggregateThreshold(AggregationPolicy::default()) } else { SweepMode::PreserveDenominations };
        self.refresh_qi_for_spend().await?;
        let summary = self.qi_summary()?;
        let head = summary.checkpoint_height.unwrap_or(0);
        let eligible: Vec<_> =
            summary.coins.iter().filter(|c| !c.reserved && u64::try_from(c.unlock_height).unwrap_or(u64::MAX) <= head + 1).collect();
        let outputs = if aggregate {
            // Threshold aggregation merges denominations up to index 6 (1 Qi) into larger coins.
            let small: u64 = eligible.iter().filter(|c| c.denomination <= 6).map(|c| c.qits).sum();
            denomination_count(U256::from(small)) + 4
        } else {
            eligible.len() + 2
        };
        if !aggregate && outputs > MAX_CHANGE_POOL {
            return Err(CoreError::Invalid(format!(
                "a denomination-preserving sweep of {} coins needs more fresh outputs than can be allocated at once; use `qi consolidate --aggregate`",
                eligible.len()
            )));
        }
        let mut attempt = 0;
        let mut pool = Some(self.change_pool(outputs.clamp(2, MAX_CHANGE_POOL))?);
        let prepared = loop {
            self.refresh_qi_for_spend().await?;
            let keys = self
                .unlocked
                .as_ref()
                .ok_or_else(|| CoreError::Locked("wallet is locked".into()))?
                .qi_keyring_with_channels(&self.qi_store)?;
            let policy = self.qi_policy(max_fee, 128);
            let prepared = QiSession::with_keys(&self.node.provider, &keys, &mut self.qi_store)
                .prepare_sweep(id, mode, policy, pool.as_mut().ok_or_else(|| CoreError::Storage("no change pool".into()))?)
                .await;
            drop(keys);
            match prepared {
                Err(e) if qi_stale(&e) && attempt < 4 => {
                    attempt += 1;
                    stale_pause().await;
                }
                other => break other,
            }
        };
        if let Some(rest) = pool.take() {
            self.release_pool(rest);
        }
        let small_coins = eligible.iter().filter(|c| c.denomination <= 6).count();
        let prepared = match prepared {
            Err(QiError::Selection(quai_sdk::wallet::SelectionError::InvalidRequest)) if aggregate => {
                return Err(CoreError::Invalid(format!(
                    "nothing to aggregate: {small_coins} spendable coin(s) of 1 Qi or less would not reduce the coin count"
                )));
            }
            other => other?,
        };
        let total: u64 = prepared.transaction().outputs.iter().map(|o| o.denomination.value()).sum();
        let mut warnings = vec![];
        if aggregate {
            warnings.push("aggregation is only accepted as the first Qi transaction in a block; it may be rejected or delayed".into());
        }
        let mut op = self.new_op(
            id,
            if aggregate { "aggregate_qi" } else { "sweep_qi" },
            "qi",
            "qi",
            "QI",
            U256::from(total),
            "self",
            serde_json::json!({}),
        );
        let digest = prepared.signing_digest().to_string();
        let outputs = prepared.transaction().outputs.len();
        let review = self.qi_review(
            op.clone(),
            if aggregate { "Aggregate Qi coins" } else { "Consolidate Qi coins" },
            "your wallet (fresh change addresses)".into(),
            prepared.transaction(),
            outputs,
            prepared.fee(),
            vec![field("Mode", if aggregate { "aggregate small denominations" } else { "preserve denominations" })],
            warnings,
            digest,
        )?;
        op.fee = prepared.fee().to_string();
        op.detail["review"] = serde_json::to_value(&review)?;
        op.detail["review_version"] = serde_json::json!(1);
        if let Err(error) = self.journal(op.clone()) {
            self.qi_store.release_unsigned(id)?;
            return Err(error);
        }
        self.pending.insert(review.op_id.clone(), Pending::Qi { prepared, op });
        Ok(review)
    }

    // ------------------------------------------------------------------ payment codes

    /// This wallet's payment code.
    pub fn payment_code(&self) -> Result<String> {
        self.meta
            .payment_code
            .clone()
            .ok_or_else(|| CoreError::Invalid("this wallet has no payment code (import a recovery phrase)".into()))
    }

    /// Registered payment-channel peers (requires unlock to validate ownership).
    pub fn peers(&self) -> Result<Vec<PeerView>> {
        let payment = self.keys()?.payment.as_ref().ok_or_else(|| CoreError::Invalid("this wallet has no payment code".into()))?;
        let contacts = self.app.contacts()?;
        let mut out = Vec::new();
        for channel in self.qi_store.payment_channels(payment)? {
            let peer = channel.channel.counterparty_code().clone();
            let code = peer.to_base58();
            let receive = self.qi_store.payment_addresses(payment, &peer, quai_sdk::payments::PaymentDirection::Receive)?.len();
            let send = self.qi_store.payment_addresses(payment, &peer, quai_sdk::payments::PaymentDirection::Send)?.len();
            out.push(PeerView {
                contact: contacts.iter().find(|c| c.payment_code.as_deref() == Some(code.as_str())).map(|c| c.name.clone()),
                code,
                receive_addresses: receive,
                send_addresses: send,
            });
        }
        Ok(out)
    }

    /// Add (`original` = None) or edit a contact: a Quai/Qi address, a payment code, or both.
    ///
    /// Formats are checked, names stay unique, and your own payment code is refused. When the
    /// wallet is unlocked a payment code is registered as a payment channel, so background sync
    /// finds payments from that contact without a separate "add peer" step.
    pub fn save_contact(
        &mut self,
        original: Option<&str>,
        name: &str,
        address: Option<&str>,
        code: Option<&str>,
        note: &str,
    ) -> Result<crate::appdb::Contact> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 64 {
            return Err(CoreError::Invalid("contact name must be 1-64 characters".into()));
        }
        let address = address.map(str::trim).filter(|a| !a.is_empty());
        let code = code.map(str::trim).filter(|c| !c.is_empty());
        if address.is_none() && code.is_none() {
            return Err(CoreError::Invalid("a contact needs a Quai/Qi address, a payment code, or both".into()));
        }
        if let Some(a) = address {
            parse_any_address(a).map_err(|_| CoreError::Invalid(format!("`{a}` is not a valid Quai or Qi address")))?;
        }
        let peer = match code {
            Some(c) => {
                let peer = PaymentCode::from_base58(c).map_err(|_| CoreError::Invalid("that payment code is not valid".into()))?;
                if self.meta.payment_code.as_deref() == Some(c) {
                    return Err(CoreError::Invalid("that is your own payment code".into()));
                }
                Some(peer)
            }
            None => None,
        };
        let taken = self.app.contact(name)?;
        match original {
            Some(orig) => {
                let mut contact = self.app.contact(orig)?.ok_or_else(|| CoreError::NotFound(format!("no contact `{orig}`")))?;
                if taken.as_ref().is_some_and(|t| t.id != contact.id) {
                    return Err(CoreError::Invalid(format!("a contact named `{name}` already exists")));
                }
                contact.name = name.to_string();
                contact.address = address.map(str::to_string);
                contact.payment_code = code.map(str::to_string);
                contact.note = note.trim().to_string();
                self.app.update_contact(&contact)?;
            }
            None => {
                if taken.is_some() {
                    return Err(CoreError::Invalid(format!("a contact named `{name}` already exists")));
                }
                self.app.add_contact(name, address, code, note.trim())?;
            }
        }
        if let Some(peer) = &peer
            && self.is_unlocked()
        {
            self.ensure_channel(peer)?;
        }
        let saved = self.app.contact(name)?.ok_or_else(|| CoreError::Storage("contact vanished after saving".into()))?;
        // The address column is where payments go; the address set is everywhere this person has
        // been seen. Saving one adds it to both, so naming an account never loses the others.
        if let Some(a) = address {
            self.app.add_contact_address(saved.id, a)?;
        }
        Ok(saved)
    }

    /// Register a peer and scan its receive channel (gap or deep continuation).
    pub async fn scan_peer(&mut self, code: &str, continue_from: Option<u32>) -> Result<(u32, usize)> {
        let peer = PaymentCode::from_base58(code.trim())?;
        self.ensure_channel(&peer)?;
        let mut options = PaymentScanOptions::default();
        if let Some(start) = continue_from {
            options.range.start = start;
        }
        let payment = self.unlocked.as_ref().and_then(|u| u.payment.as_ref()).ok_or_else(no_payment)?;
        for attempt in 0..5 {
            match scan_payment_channel(&self.node.provider, &mut self.qi_store, payment, &peer, &options, || false).await {
                Ok(report) => return Ok((report.next_index, report.indexes.len())),
                Err(e) if qi_stale(&e) && attempt < 4 => stale_pause().await,
                Err(e) => return Err(e.into()),
            }
        }
        Err(CoreError::Network("scan kept crossing block boundaries".into()))
    }

    /// A complete pass over the Pelagus mailbox, from its first announcement: what `payment
    /// discover` and the TUI's "scan mailbox" run.
    pub async fn discover_mailbox(&mut self) -> Result<MailboxSummary> {
        self.discover_mailbox_pass(MailboxPass::Full, &mut || false).await
    }

    /// Read the mailbox's announcements and probe the senders in them, without registering any.
    ///
    /// Announcements are unauthenticated: anyone can announce a payment code to anyone for one
    /// cheap transaction, and a sender that wants to look real only has to leave dust on the
    /// channel. So a sender becomes a channel only when the user accepts it. An announced sender
    /// this wallet has not registered is probed (a few addresses, nothing persisted by the SDK)
    /// and, if the probe finds Qi waiting, recorded as a [`ChannelOffer`] for the user to accept
    /// or decline. Channels already registered are rescanned in full by the same page.
    ///
    /// The mailbox only appends, so a cursor keeps each pass to what is new; [`MailboxPass::Due`]
    /// rewinds it to the start every [`MAILBOX_REPROBE_SECS`], so a sender who announced before
    /// paying is looked at again. A pass reads at most [`MAILBOX_PAGES_PER_PASS`] pages and stops
    /// between pages when `stop` says so; the next pass carries on from there.
    pub async fn discover_mailbox_pass(&mut self, pass: MailboxPass, stop: &mut dyn FnMut() -> bool) -> Result<MailboxSummary> {
        let mailbox_address = self.quai_contract(&self.network.mailbox, "payment mailbox")?;
        let caller = self.caller()?;
        let own = self.unlocked.as_ref().and_then(|u| u.payment.as_ref()).ok_or_else(no_payment)?.public_code().to_base58();
        let network = self.network.id.clone();
        let at = crate::registry::now();
        let cursor_key = format!("mailbox_cursor:{network}");
        let rewound_key = format!("mailbox_rewound:{network}");
        let rewind = match pass {
            MailboxPass::Full => true,
            MailboxPass::Due => {
                self.app.kv(&rewound_key)?.and_then(|v| v.parse::<u64>().ok()).is_none_or(|t| at.saturating_sub(t) >= MAILBOX_REPROBE_SECS)
            }
        };
        let mut start = if rewind { 0 } else { self.app.kv(&cursor_key)?.and_then(|v| v.parse::<usize>().ok()).unwrap_or(0) };
        if rewind {
            self.app.set_kv(&rewound_key, &at.to_string())?;
        }
        let mut summary = MailboxSummary::default();
        let pages = if pass == MailboxPass::Full { usize::MAX } else { MAILBOX_PAGES_PER_PASS };
        for page in 0..pages {
            if page > 0 && stop() {
                break;
            }
            let report = self.mailbox_page(mailbox_address, caller, start).await?;
            // Announcements that are not payment codes are reported with every page; ours is
            // one when someone announces us to ourselves, and it took a place in the page.
            summary.invalid = report.invalid.iter().filter(|c| **c != own).count();
            let taken = report.scanned.len() + report.invalid.iter().filter(|c| **c == own).count();
            for scan in &report.scanned {
                let code = scan.sender.to_base58();
                summary.senders.push(code.clone());
                match record_announcement(&self.app, &network, &code, scan.registration, scan.found, at)? {
                    Announced::Channel => summary.registered.push(code),
                    Announced::Offer(offer) => summary.new_offers.push(offer),
                    Announced::Refused => summary.refused += 1,
                    Announced::Quiet => {}
                }
            }
            summary.deferred = report.deferred.len();
            match report.next_start {
                Some(next) => start = next,
                None => {
                    start += taken;
                    summary.deferred = 0;
                    break;
                }
            }
        }
        self.app.set_kv(&cursor_key, &start.to_string())?;
        summary.pending = self.channel_offers()?.len();
        Ok(summary)
    }

    /// One page of mailbox discovery, report-only, retried while the chain moves under it.
    async fn mailbox_page(
        &mut self,
        mailbox_address: QuaiAddress,
        caller: QuaiAddress,
        start: usize,
    ) -> Result<quai_sdk::payment_channels::MailboxDiscoveryReport> {
        let payment = self.unlocked.as_ref().and_then(|u| u.payment.as_ref()).ok_or_else(no_payment)?;
        let mailbox = PaymentMailbox::new(mailbox_address, &self.node.provider)?;
        let request = MailboxDiscovery::new(start, MAILBOX_PAGE).with_registration(MailboxRegistration::ReportOnly);
        for attempt in 0..5 {
            match discover_mailbox_channels(&self.node.provider, &mut self.qi_store, payment, &mailbox, caller, &request, || false).await {
                Ok(r) => return Ok(r),
                Err(e) if qi_stale(&e) && attempt < 4 => stale_pause().await,
                Err(e) => return Err(e.into()),
            }
        }
        Err(CoreError::Network("mailbox scan kept crossing block boundaries".into()))
    }

    /// Senders who announced a channel with Qi waiting on it and are not registered yet, oldest
    /// first. The order does not follow the amounts, which change with every probe, so a list the
    /// user is choosing from does not reshuffle under the cursor.
    pub fn channel_offers(&self) -> Result<Vec<ChannelOffer>> {
        let mut offers: Vec<ChannelOffer> =
            self.app.kv_prefix(&offer_key(&self.network.id, ""))?.into_iter().filter_map(|(_, v)| serde_json::from_str(&v).ok()).collect();
        offers.sort_by(|a, b| a.first_seen.cmp(&b.first_seen).then(a.code.cmp(&b.code)));
        Ok(offers)
    }

    /// Accept an offered channel: register it and scan it in full, so its Qi joins the wallet.
    /// Returns the scan's (next index, addresses found) like [`Self::scan_peer`].
    pub async fn accept_channel_offer(&mut self, code: &str) -> Result<(u32, usize)> {
        let code = code.trim();
        if self.app.kv(&offer_key(&self.network.id, code))?.is_none() {
            return Err(CoreError::NotFound(format!("no channel offer from {}", crate::session::short_code(code))));
        }
        // Registering clears the offer (every way a channel is registered does).
        self.scan_peer(code, None).await
    }

    /// Decline an offered channel: forget the offer and stop offering it.
    pub fn decline_channel_offer(&self, code: &str) -> Result<()> {
        let code = code.trim();
        let key = offer_key(&self.network.id, code);
        if self.app.kv(&key)?.is_none() {
            return Err(CoreError::NotFound(format!("no channel offer from {}", crate::session::short_code(code))));
        }
        self.app.delete_kv(&key)?;
        self.app.set_kv(&declined_key(&self.network.id, code), &crate::registry::now().to_string())
    }

    /// Background payment-code sync, so private payments arrive without knowing the sender.
    ///
    /// BIP47 payments land on addresses only the two parties can derive, so an unknown sender
    /// is found through its mailbox announcement: every announced channel is registered and
    /// scanned. Channels registered another way (peers added by hand, or senders who shared
    /// their code but never notified) are rescanned too, so later payments past the scanned
    /// range are found. Needs unlocked keys (the private payment code); a no-op otherwise.
    pub async fn sync_payment_channels(&mut self) -> Result<PaymentSync> {
        let sync = self.sync_payment_channels_until(&mut || false).await?;
        if sync.scanned > 0 {
            self.refresh_qi().await?;
        }
        Ok(sync)
    }

    /// The same sync, abandoned between channels when `stop` says something else is waiting.
    ///
    /// Each channel is a chain scan, so a wallet with several peers spends seconds here — long
    /// enough that a user switching wallets would sit behind it. What is scanned before stopping
    /// is kept: the next sync picks the rest up, because every channel records its own progress.
    ///
    /// It does not refresh Qi afterwards; the caller does, where it suits it. The TUI hands that
    /// refresh to its Qi lane so it never holds up the wallet worker, and
    /// [`Self::sync_payment_channels`] runs it inline for the CLI and the daemon.
    pub async fn sync_payment_channels_until(&mut self, stop: &mut dyn FnMut() -> bool) -> Result<PaymentSync> {
        let mut sync = PaymentSync::default();
        let has_payment = self.unlocked.as_ref().is_some_and(|u| u.payment.is_some());
        if !has_payment || stop() {
            return Ok(sync);
        }
        let mut covered = std::collections::HashSet::new();
        if self.network.mailbox.is_some() {
            let summary = self.discover_mailbox_pass(MailboxPass::Due, stop).await?;
            // Registered channels in the pages read were rescanned in full there.
            sync.scanned += summary.registered.len();
            covered.extend(summary.registered);
            sync.new_offers = summary.new_offers;
            sync.deferred = summary.deferred;
        }
        let registered: Vec<String> = {
            let payment = self.unlocked.as_ref().and_then(|u| u.payment.as_ref()).ok_or_else(no_payment)?;
            self.qi_store.payment_channels(payment)?.iter().map(|c| c.channel.counterparty_code().to_base58()).collect()
        };
        for code in registered.into_iter().filter(|c| !covered.contains(c)) {
            if stop() {
                sync.stopped = true;
                break;
            }
            self.scan_peer(&code, None).await?;
            sync.scanned += 1;
        }
        Ok(sync)
    }

    /// Whether our code is announced to `peer` in the mailbox (None when no mailbox).
    pub async fn peer_notified(&self, peer: &PaymentCode) -> Result<Option<bool>> {
        let Some(address) = self.network.mailbox.as_ref() else {
            return Ok(None);
        };
        let Some(ours) = self.meta.payment_code.as_ref() else {
            return Ok(None);
        };
        let ours = PaymentCode::from_base58(ours)?;
        let mailbox = PaymentMailbox::new(address.parse().map_err(|_| CoreError::Invalid("mailbox address".into()))?, &self.node.provider)?;
        Ok(Some(mailbox.is_notified(self.caller()?, &ours, peer, BlockTag::Latest).await?))
    }

    /// Review a mailbox `notify(ourCode, peerCode)` from a Quai account.
    pub async fn review_notify(&mut self, account: Option<&str>, peer: &str, max_fee: Option<&str>) -> Result<Review> {
        let peer = match self.resolve_recipient(peer)? {
            Recipient::PaymentCode(c) => c,
            _ => return Err(CoreError::Invalid("notify needs a payment code or a contact with one".into())),
        };
        let mailbox_address = self.quai_contract(&self.network.mailbox, "payment mailbox")?;
        let ours = PaymentCode::from_base58(&self.payment_code()?)?;
        let from = self.account(account)?;
        let mut warnings = vec!["announcements are public: this links your payment code to the recipient's on-chain".into()];
        if self.peer_notified(&peer).await?.unwrap_or(false) {
            warnings.push("the recipient is already notified; this transaction is not needed".into());
        }
        let call = PaymentMailbox::new(mailbox_address, &self.node.provider)?.notify(&ours, &peer)?;
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: "notify".into(),
            title: "Notify payment-code recipient".into(),
            asset: "QUAI".into(),
            amount: U256::ZERO,
            decimals: QUAI_DECIMALS,
            counterparty: peer.to_base58(),
            fields: vec![field("Mailbox", mailbox_address.to_string()), field("Recipient code", peer.to_base58())],
            warnings,
            detail: serde_json::json!({"peer": peer.to_base58()}),
            max_gas: 600_000,
            max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
        })
        .await
    }

    /// Review posting a message to the on-chain board. Every message is public and permanent:
    /// the review says so, because nothing can take one back once it is mined.
    pub async fn review_post(&mut self, account: Option<&str>, channel: &str, text: &str, max_fee: Option<&str>) -> Result<Review> {
        let tag = crate::messages::channel_tag(channel)?;
        let body = text.as_bytes().to_vec();
        self.review_board(
            account,
            &tag,
            crate::messages::KIND_TEXT,
            body,
            format!("#{channel}"),
            vec![field("Channel", format!("#{channel}")), field("Message", text.to_string())],
            vec![
                "messages are public and permanent: anyone can read this, and nothing can take it back".into(),
                "it is signed by this account, which links the message to your address".into(),
            ],
            serde_json::json!({"channel": channel}),
            max_fee,
        )
        .await
    }

    /// The sealed conversation with a peer: their payment code (or a contact holding one) against
    /// this wallet's payment account. Both sides derive the same one, so it needs no setup and no
    /// message from them first.
    pub fn conversation_with(&self, peer: &str) -> Result<crate::messages::Conversation> {
        let peer = match self.resolve_recipient(peer)? {
            Recipient::PaymentCode(c) => c,
            _ => return Err(CoreError::Invalid("a sealed message needs a payment code, or a contact who has one".into())),
        };
        let keys = self.keys()?;
        let payment = keys.payment.as_ref().ok_or_else(no_payment)?;
        crate::messages::conversation(payment, &peer)
    }

    /// The sealed conversation with a peer, oldest first, already opened. Reading needs this
    /// wallet's payment key, so it is unlocked work — the key never leaves the session.
    pub async fn read_conversation(&self, peer: &str, blocks: u64) -> Result<Vec<SealedLine>> {
        let conversation = self.conversation_with(peer)?;
        let ctx = self.data_ctx()?;
        let mut posts = crate::messages::conversation_posts(&ctx, &conversation, blocks).await?;
        posts.reverse();
        let mine: Vec<String> = self.meta.quai_owner_addresses().iter().map(|a| a.to_lowercase()).collect();
        // The accounts this contact is known by, to point out a post from any other one.
        let known: Vec<String> = self
            .contact_for_code(peer)
            .map(|c| {
                let mut all: Vec<String> = c.address.iter().cloned().collect();
                all.extend(self.app.contact_addresses(c.id).unwrap_or_default());
                all.into_iter().map(|a| a.to_lowercase()).collect()
            })
            .unwrap_or_default();
        Ok(crate::messages::sealed_lines(&conversation, &posts, &mine, &known))
    }

    /// The contact holding a payment code, given the code or a contact's name.
    fn contact_for_code(&self, peer: &str) -> Option<crate::appdb::Contact> {
        let contacts = self.app.contacts().ok()?;
        let code = match self.resolve_recipient(peer) {
            Ok(Recipient::PaymentCode(c)) => c.to_base58(),
            _ => return None,
        };
        contacts.into_iter().find(|c| c.payment_code.as_deref() == Some(code.as_str()))
    }

    /// Review a sealed message to one peer. The body is encrypted before it is shown, and the
    /// review says exactly how far that protection goes.
    pub async fn review_dm(&mut self, account: Option<&str>, peer: &str, text: &str, max_fee: Option<&str>) -> Result<Review> {
        let conversation = self.conversation_with(peer)?;
        let (tag, body) = crate::messages::seal(&conversation, text, self.head().await?)?;
        let who = crate::session::short_code(peer);
        self.review_board(
            account,
            &tag,
            crate::messages::KIND_SEALED,
            body,
            who.clone(),
            vec![field("To", who), field("Message", text.to_string()), field("Sealed", "only the two of you can read it")],
            vec![
                "the message is encrypted, but this transaction is not hidden: your address and the time are public, and its size to within a bucket".into(),
                "anyone holding either side's notification key can read the whole conversation".into(),
                "it cannot be taken back once it is mined".into(),
            ],
            serde_json::json!({"sealed": true}),
            max_fee,
        )
        .await
    }

    /// Post one body to the board. Shared by public channels and sealed conversations, so both
    /// go through the same review and the same checks the contract makes.
    #[allow(clippy::too_many_arguments)]
    async fn review_board(
        &mut self,
        account: Option<&str>,
        tag: &[u8; 32],
        kind: u8,
        body: Vec<u8>,
        counterparty: String,
        mut fields: Vec<Field>,
        warnings: Vec<String>,
        mut detail: serde_json::Value,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        use quai_sdk::contracts::Contract;
        let pin = self
            .network
            .ecosystem
            .messages
            .clone()
            .ok_or_else(|| CoreError::NotFound(format!("no message board is configured on {}", self.network.id)))?;
        let address = crate::data::verify_pinned(&self.app, &self.node, &self.network, &pin, "message board", Trust::FirstHand).await?;
        let args = crate::messages::post_args(tag, kind, &body)?;
        let from = self.account(account)?;
        let contract = Contract::new(address, crate::messages::interface()?, &self.node.provider);
        let call = contract.prepare("post", &args, U256::ZERO)?;
        fields.push(field("Size", format!("{} bytes", body.len())));
        fields.push(field("Board", format!("{address} · {}", pin.trust_label())));
        detail["bytes"] = serde_json::json!(body.len());
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: "board_post".into(),
            title: if kind == crate::messages::KIND_SEALED { "Send a sealed message".into() } else { "Post a message".into() },
            asset: "QUAI".into(),
            amount: U256::ZERO,
            decimals: QUAI_DECIMALS,
            counterparty,
            fields,
            warnings,
            detail,
            max_gas: 200_000,
            max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
        })
        .await
    }

    // ------------------------------------------------------------------ conversions

    /// Quote a conversion with batch-discount scenarios; this path requires no signing keys.
    pub async fn conversion_quote(&self, direction: &str, value: &str) -> Result<ConversionQuote> {
        let ctx = self.data_ctx()?;
        let qi = self.qi_receive_addresses().ok().and_then(|rows| rows.first().map(|(_, address, _)| address.clone()));
        quote_conversion(&ctx, direction, value, self.meta.quai_accounts.first().map(|a| a.address.as_str()), qi.as_deref()).await
    }

    /// Review a QUAI → Qi conversion from an account into a fresh Qi address.
    pub async fn review_convert_quai_to_qi(
        &mut self,
        from: Option<&str>,
        value: &str,
        slippage_bps: u16,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        let from = self.account(from)?;
        let its = amount::parse_quai(value)?;
        if its < U256::from(quai_sdk::consensus::MIN_QUAI_CONVERSION_VALUE) {
            return Err(CoreError::Invalid(format!(
                "the minimum conversion is {} QUAI",
                amount::quai(U256::from(quai_sdk::consensus::MIN_QUAI_CONVERSION_VALUE))
            )));
        }
        ConversionSlippage::new(slippage_bps)?;
        let destination = self.new_qi_address(Some("conversion"))?;
        let quote = self.conversion_quote("quai_to_qi", value).await.ok();
        // Refused inside the EVM during a hold window, which means mined, nonce consumed and gas
        // burned for a conversion that cannot happen. Nothing is gained by letting it through, and
        // the same funds convert for free once the window passes, so this one is a refusal rather
        // than a warning. Mainnet is past both windows and can never re-enter one.
        if let Some(h) = quote.as_ref().and_then(|q| q.hold.as_ref()).filter(|h| h.burns_gas) {
            return Err(CoreError::Invalid(format!("{} Try again after that height.", h.note)));
        }
        let (fields, mut warnings) = match &quote {
            Some(q) => conversion_review_context(q, slippage_bps, "Qi", amount::QI_DECIMALS),
            None => (Vec::new(), Vec::new()),
        };
        warnings.push("converted Qi is locked for about two weeks (block-height based)".into());
        // The conversion session builds its own payload; this intent is journal context only.
        let own = Self::quai_address_of(&from)?;
        let req = AccountRequest {
            from,
            intent: AccountIntent {
                to: own,
                value: its,
                data: RpcData::new(vec![]).map_err(|_| CoreError::Invalid("data".into()))?,
                access_list: vec![],
            },
            kind: "convert_quai_to_qi".into(),
            title: "Convert QUAI → Qi".into(),
            asset: "QUAI".into(),
            amount: its,
            decimals: QUAI_DECIMALS,
            counterparty: destination.to_string(),
            fields,
            warnings,
            detail: serde_json::json!({
                "destination": destination.to_string(),
                "slippage_bps": slippage_bps,
                "quoted_qits": quote.as_ref().and_then(|q| q.quoted.clone()),
                "expected_qits": quote.as_ref().and_then(|q| q.expected.clone()),
            }),
            max_gas: 1_000_000,
            max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
        };
        self.prepare_quai_conversion(req, destination, slippage_bps).await
    }

    /// Review a Qi → QUAI conversion into an account.
    pub async fn review_convert_qi_to_quai(
        &mut self,
        to_account: Option<&str>,
        value: &str,
        slippage_bps: u16,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        let account = self.account(to_account)?;
        let destination = Self::quai_address_of(&account)?;
        let qits = amount::parse_qi(value)?;
        let slippage = ConversionSlippage::new(slippage_bps)?;
        let refund = self.new_qi_address(Some("conversion refund"))?;
        let intent = QiSpecialIntent::Conversion(QiConversionIntent { destination, refund, slippage });
        let quote = self.conversion_quote("qi_to_quai", value).await.ok();
        let (quote_fields, mut warnings) = match &quote {
            Some(q) => conversion_review_context(q, slippage_bps, "QUAI", QUAI_DECIMALS),
            None => (Vec::new(), Vec::new()),
        };
        let mut fields =
            vec![field("Destination account", format!("{} ({})", destination, account.label)), field("Refund address", refund.to_string())];
        fields.extend(quote_fields);
        warnings.push("converted QUAI is locked for about two weeks (block-height based)".into());
        if qits < U256::from(1000u64) {
            warnings.push("refunds below 1 Qi may create no outputs".into());
        }
        self.prepare_qi_special(
            intent,
            qits,
            max_fee,
            "convert_qi_to_quai",
            "Convert Qi → QUAI",
            destination.to_string(),
            fields,
            warnings,
            serde_json::json!({
                "destination": destination.to_string(),
                "refund": refund.to_string(),
                "slippage_bps": slippage_bps,
                "quoted_its": quote.as_ref().and_then(|q| q.quoted.clone()),
                "expected_its": quote.as_ref().and_then(|q| q.expected.clone()),
            }),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn prepare_qi_special(
        &mut self,
        intent: QiSpecialIntent,
        qits: U256,
        max_fee: Option<&str>,
        kind: &str,
        title: &str,
        to: String,
        mut fields: Vec<Field>,
        warnings: Vec<String>,
        detail: serde_json::Value,
    ) -> Result<Review> {
        self.require_execution_source()?;
        let cap = self.parse_fee_cap(max_fee, QI_DECIMALS)?;
        let id = new_operation_id()?;
        let _operation_guard = self.operation_lock(id)?;
        let estimated = self.network.specialized_fee_estimation;
        self.refresh_qi_for_spend().await?;
        let mut pool_size = self.estimate_change(qits, cap.unwrap_or(U256::from(500u64)))?;
        let mut stale = 0;
        let mut pool = Some(self.change_pool(pool_size)?);
        let prepared = loop {
            self.refresh_qi_for_spend().await?;
            let keys = self
                .unlocked
                .as_ref()
                .ok_or_else(|| CoreError::Locked("wallet is locked".into()))?
                .qi_keyring_with_channels(&self.qi_store)?;
            let policy = QiPolicy {
                initial_fee: U256::ZERO,
                max_fee: cap.unwrap_or(U256::from(500u64)),
                max_inputs: 64,
                max_outputs: 256,
                max_fee_rounds: 12,
                max_snapshot_age: 10,
            };
            let mut session = QiSession::with_keys(&self.node.provider, &keys, &mut self.qi_store);
            let change = pool.as_mut().ok_or_else(|| CoreError::Storage("no change pool".into()))?;
            let result = if estimated {
                session.prepare_special_estimated(id, qits, intent, QiFeeProfile::V056ShaAnchored, policy, change).await
            } else {
                let fee = cap.unwrap_or(U256::from(100u64));
                session.prepare_special(id, qits, intent, fee, policy, change).await
            };
            drop(session);
            drop(keys);
            match result {
                Ok(p) => break Ok(p),
                Err(e) if qi_stale(&e) && stale < 4 => {
                    stale += 1;
                    stale_pause().await;
                }
                Err(QiError::InsufficientChange) if pool_size < 256 => {
                    pool_size = (pool_size * 2).min(256);
                    if let Some(small) = pool.take() {
                        self.release_pool(small);
                    }
                    pool = Some(self.change_pool(pool_size)?);
                }
                Err(e) => break Err(CoreError::from(e)),
            }
        };
        if let Some(rest) = pool.take() {
            self.release_pool(rest);
        }
        let prepared = prepared?;
        fields.push(field(
            "Fee source",
            if estimated { "node estimate (v0.56 profile)" } else { "explicit fee (node estimator not qualified on this network)" },
        ));
        let data = prepared.transaction().transaction().data.clone();
        fields.push(field("Special data", format!("0x{}", hex::encode(&data))));
        let mut op = self.new_op(id, kind, "qi", "qi", "QI", qits, &to, detail);
        op.fee = prepared.fee().to_string();
        let digest = prepared.transaction().signing_digest()?.to_string();
        let tx = prepared.transaction().transaction().clone();
        let review = self.qi_review(op.clone(), title, to, &tx, 0, prepared.fee(), fields, warnings, digest)?;
        op.fee = prepared.fee().to_string();
        op.detail["review"] = serde_json::to_value(&review)?;
        op.detail["review_version"] = serde_json::json!(1);
        if let Err(error) = self.journal(op.clone()) {
            self.qi_store.release_unsigned(id)?;
            return Err(error);
        }
        self.pending.insert(review.op_id.clone(), Pending::QiSpecial { prepared, op });
        Ok(review)
    }

    // ------------------------------------------------------------------ wrapping

    /// Wrapped asset balances for an account.
    pub async fn wrap_status(&self, account: Option<&str>) -> Result<WrapStatus> {
        let owner = match (self.account(account), account) {
            (Ok(a), _) => Self::quai_address_of(&a)?,
            // Watch-only wallets: show the first watched Quai address.
            (Err(e), None) => match self.quai_owner_addresses().first() {
                Some(a) => a.parse().map_err(|_| CoreError::Invalid(format!("invalid watched address {a}")))?,
                None => return Err(e),
            },
            (Err(e), Some(_)) => return Err(e),
        };
        if self.network.wqi.is_none() && self.network.wquai.is_none() {
            return Err(CoreError::Network(format!("wrapper contracts are not configured on {}", self.network.id)));
        }
        let mut status = WrapStatus { account: owner.to_string(), wqi_atoms: None, wqi_qi: None, unclaimed_qits: None, wquai_atoms: None };
        if let Some(addr) = &self.network.wqi {
            let wqi = WrappedQi::new(addr.parse().map_err(|_| CoreError::Invalid("WQI address".into()))?, self.provider())?;
            let atoms = wqi.token()?.balance_of(owner, owner, BlockTag::Latest).await?;
            status.wqi_qi = Some(amount::qi(quai_sdk::wrappers::wqi_atoms_to_qits(atoms).unwrap_or(U256::ZERO)));
            status.wqi_atoms = Some(atoms.to_string());
            status.unclaimed_qits = Some(wqi.unclaimed(owner, BlockTag::Latest).await?.to_string());
        }
        if let Some(addr) = &self.network.wquai {
            let wquai = WrappedQuai::new(addr.parse().map_err(|_| CoreError::Invalid("WQUAI address".into()))?, self.provider())?;
            status.wquai_atoms = Some(wquai.token()?.balance_of(owner, owner, BlockTag::Latest).await?.to_string());
        }
        Ok(status)
    }

    /// Review wrapping Qi into WQI backing for an account (claim separately).
    pub async fn review_wrap_qi(&mut self, beneficiary: Option<&str>, value: &str, max_fee: Option<&str>) -> Result<Review> {
        let account = self.account(beneficiary)?;
        let destination = Self::quai_address_of(&account)?;
        let (wqi, trust) = self.wrapper_contract(true).await?;
        let qits = amount::parse_qi(value)?;
        let intent = QiSpecialIntent::Wrapping(QiWrappingIntent { destination, owner_contract: wqi });
        self.prepare_qi_special(
            intent,
            qits,
            max_fee,
            "wrap_qi",
            "Wrap Qi → WQI (step 1 of 2)",
            destination.to_string(),
            vec![
                field("Beneficiary account", format!("{destination} ({})", account.label)),
                field("WQI contract", format!("{wqi} ({trust})")),
            ],
            vec!["after the deposit settles, claim your WQI with `wrap claim` (a Quai transaction)".into()],
            serde_json::json!({"beneficiary": destination.to_string(), "contract": wqi.to_string()}),
        )
        .await
    }

    /// Review claiming WQI from settled wrapping backing.
    pub async fn review_claim_wqi(&mut self, account: Option<&str>, max_fee: Option<&str>) -> Result<Review> {
        let from = self.account(account)?;
        let owner = Self::quai_address_of(&from)?;
        let (contract, trust) = self.wrapper_contract(true).await?;
        let wqi = WrappedQi::new(contract, &self.node.provider)?;
        let unclaimed = wqi.unclaimed(owner, BlockTag::Latest).await?;
        if unclaimed.is_zero() {
            return Err(CoreError::Invalid("no unclaimed wrapped Qi for this account (the deposit may not have settled yet)".into()));
        }
        let call = wqi.claim_deposit()?;
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: "claim_wqi".into(),
            title: "Claim WQI (step 2 of 2)".into(),
            asset: "QI".into(),
            amount: unclaimed,
            decimals: QI_DECIMALS,
            counterparty: contract.to_string(),
            fields: vec![
                field("Unclaimed backing", format!("{} Qi", amount::qi(unclaimed))),
                field("WQI contract", format!("{contract} ({trust})")),
            ],
            warnings: vec![],
            detail: serde_json::json!({"contract": contract.to_string(), "recipient": owner.to_string(), "to_token": contract.to_string(),
                "financial_effects": [{"direction": "in", "asset": "WQI", "token": contract.to_string(), "decimals": 18,
                "amount": quai_sdk::wrappers::qits_to_wqi_atoms(unclaimed)?.to_string(), "estimated": true,
                "note": "claimable backing observed before preparation; actual receipt determines continuation"}]}),
            max_gas: 300_000,
            max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
        })
        .await
    }

    /// Review redeeming WQI to a fresh Qi address.
    pub async fn review_unwrap_wqi(&mut self, account: Option<&str>, value: &str, max_fee: Option<&str>) -> Result<Review> {
        let from = self.account(account)?;
        let owner = Self::quai_address_of(&from)?;
        let (contract, trust) = self.wrapper_contract(true).await?;
        let qits = amount::parse_qi(value)?;
        let plan = QiRedemptionPlan::go_quai_v056(qits)?;
        if !plan.discarded_qits.is_zero() {
            return Err(CoreError::Invalid(format!(
                "redemptions must be whole multiples of 1 Qi; {} Qi would be discarded as dust",
                amount::qi(plan.discarded_qits)
            )));
        }
        let atoms = qits_to_wqi_atoms(qits)?;
        let balance = WrappedQi::new(contract, &self.node.provider)?.token()?.balance_of(owner, owner, BlockTag::Latest).await?;
        if balance < atoms {
            return Err(CoreError::Insufficient(format!(
                "WQI balance is {} Qi",
                amount::qi(quai_sdk::wrappers::wqi_atoms_to_qits(balance).unwrap_or(U256::ZERO))
            )));
        }
        let beneficiary = self.new_qi_address(Some("WQI redemption"))?;
        let etx_gas = plan.minimum_etx_gas.saturating_mul(2).max(30_000);
        let call = WrappedQi::new(contract, &self.node.provider)?.unwrap(beneficiary, qits, etx_gas)?;
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: "unwrap_wqi".into(),
            title: "Unwrap WQI → Qi".into(),
            asset: "QI".into(),
            amount: qits,
            decimals: QI_DECIMALS,
            counterparty: beneficiary.to_string(),
            fields: vec![
                field("WQI contract", format!("{contract} ({trust})")),
                field("Qi beneficiary", beneficiary.to_string()),
                field("WQI atoms", atoms.to_string()),
                field("Destination gas", etx_gas.to_string()),
            ],
            warnings: vec!["redeemed Qi is locked for a short period before it can be spent".into()],
            detail: serde_json::json!({"beneficiary": beneficiary.to_string(), "contract": contract.to_string(), "financial_effects": [
                {"direction":"out","asset":"WQI","token":contract.to_string(),"decimals":18,"amount":atoms.to_string(),"note":"burned wrapped tokens"},
                {"direction":"in","asset":"Qi","token":"qi","decimals":3,"amount":qits.to_string(),"estimated":true,"note":"destination credit pending protocol confirmation and maturity"}
            ]}),
            // Each redeemed denomination creates an outpoint; Pelagus allows 1.1M for large redemptions.
            max_gas: 1_100_000,
            max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
        })
        .await
    }

    /// Review wrapping QUAI into WQUAI.
    pub async fn review_wrap_quai(&mut self, account: Option<&str>, value: &str, max_fee: Option<&str>) -> Result<Review> {
        let from = self.account(account)?;
        let (contract, trust) = self.wrapper_contract(false).await?;
        let its = amount::parse_quai(value)?;
        let call = WrappedQuai::new(contract, &self.node.provider)?.deposit(its)?;
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: "wrap_quai".into(),
            title: "Wrap QUAI → WQUAI".into(),
            asset: "QUAI".into(),
            amount: its,
            decimals: QUAI_DECIMALS,
            counterparty: contract.to_string(),
            fields: vec![field("WQUAI contract", format!("{contract} ({trust})"))],
            warnings: vec![],
            detail: serde_json::json!({"contract": contract.to_string(), "financial_effects": [
                {"direction":"out","asset":"QUAI","token":"quai","decimals":18,"amount":its.to_string()},
                {"direction":"in","asset":"WQUAI","token":contract.to_string(),"decimals":18,"amount":its.to_string(),"note":"1:1 wrapped native coin"}
            ]}),
            max_gas: 200_000,
            max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
        })
        .await
    }

    /// Review unwrapping WQUAI into QUAI.
    pub async fn review_unwrap_quai(&mut self, account: Option<&str>, value: &str, max_fee: Option<&str>) -> Result<Review> {
        let from = self.account(account)?;
        let owner = Self::quai_address_of(&from)?;
        let (contract, trust) = self.wrapper_contract(false).await?;
        let atoms = amount::parse_quai(value)?;
        let wquai = WrappedQuai::new(contract, &self.node.provider)?;
        let balance = wquai.token()?.balance_of(owner, owner, BlockTag::Latest).await?;
        if balance < atoms {
            return Err(CoreError::Insufficient(format!("WQUAI balance is {}", amount::quai(balance))));
        }
        let call = wquai.withdraw(atoms)?;
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: "unwrap_quai".into(),
            title: "Unwrap WQUAI → QUAI".into(),
            asset: "WQUAI".into(),
            amount: atoms,
            decimals: QUAI_DECIMALS,
            counterparty: contract.to_string(),
            fields: vec![field("WQUAI contract", format!("{contract} ({trust})"))],
            warnings: vec![],
            detail: serde_json::json!({"contract": contract.to_string(), "financial_effects": [
                {"direction":"out","asset":"WQUAI","token":contract.to_string(),"decimals":18,"amount":atoms.to_string()},
                {"direction":"in","asset":"QUAI","token":"quai","decimals":18,"amount":atoms.to_string(),"note":"1:1 native redemption"}
            ]}),
            max_gas: 200_000,
            max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
        })
        .await
    }

    // ------------------------------------------------------------------ keys

    /// Import a private key into this (unlocked) wallet. The vault is re-sealed, so the password
    /// is asked for again and checked against the vault first: a mistyped one must not become the
    /// vault's new password.
    pub fn import_key(&mut self, password: &str, secret_hex: &str, label: &str) -> Result<String> {
        if self.unlocked.is_none() {
            return Err(CoreError::Locked("wallet is locked".into()));
        }
        let mut meta = self.meta.clone();
        let keys = self.unlocked.as_mut().ok_or_else(|| CoreError::Locked("wallet is locked".into()))?;
        let address = self.registry.add_key(&mut meta, keys, password, secret_hex, label)?;
        self.meta = meta;
        self.sync_metadata()?;
        Ok(address.to_string())
    }

    /// Recovery phrase and passphrase (requires re-entering the password).
    pub fn export_mnemonic(&self, password: &str) -> Result<(zeroize::Zeroizing<String>, zeroize::Zeroizing<String>)> {
        let unlocked = self.registry.unlock(&self.meta, password)?;
        let m = unlocked.secrets().mnemonic.as_ref().ok_or_else(|| CoreError::Invalid("this wallet has no recovery phrase".into()))?;
        Ok((zeroize::Zeroizing::new(m.phrase.clone()), zeroize::Zeroizing::new(m.passphrase.clone())))
    }

    /// Private key hex for an address (requires re-entering the password).
    pub fn export_private_key(&self, password: &str, address: &str) -> Result<zeroize::Zeroizing<String>> {
        let unlocked = self.registry.unlock(&self.meta, password)?;
        let parsed = parse_any_address(address)?;
        let key = match self.meta.quai_accounts.iter().find(|a| a.address.eq_ignore_ascii_case(address)) {
            Some(a) => unlocked.quai_key(parsed, a.hd_index)?,
            None => match self.qi_store.addresses()?.into_iter().find(|p| p.address() == parsed) {
                Some(p) => match p.origin() {
                    quai_sdk::wallet::storage::KeyOrigin::Bip44 { change, index, .. } => unlocked
                        .qi_hd
                        .as_ref()
                        .ok_or_else(|| CoreError::Invalid("no Qi HD root".into()))?
                        .derive_key(0, change, index)?
                        .secret_key()?,
                    quai_sdk::wallet::storage::KeyOrigin::ImportedPublic => unlocked.imported_key(parsed)?,
                },
                None => return Err(CoreError::NotFound(format!("{address} is not a key in this wallet"))),
            },
        };
        if key.public_key().address() != parsed {
            return Err(CoreError::Invalid("key does not match address".into()));
        }
        Ok(zeroize::Zeroizing::new(format!("0x{}", hex::encode(key.export_bytes().as_bytes()))))
    }
}

/// A warning when the protocol's discount takes a large share of a conversion: the spot rate
/// promises far more than arrives once the block's conversion flow is counted, and at this size
/// the difference is the conversion's own doing, not bad luck.
/// The fields and warnings both conversion reviews show, so the two directions cannot drift apart.
///
/// Ordered by what decides the answer: what arrives, what the discount costs at this instant, and
/// only then the rate and the batch scenarios. The tolerance is checked against **both** figures the
/// node offers — the batch model and its own estimate — because either one exceeding it is a refund.
fn conversion_review_context(q: &ConversionQuote, slippage_bps: u16, unit: &str, decimals: u8) -> (Vec<Field>, Vec<String>) {
    let mut fields = Vec::new();
    let mut warnings = Vec::new();
    if let Some(shown) = &q.expected_display {
        fields.push(field("You receive (estimate)", shown.clone()));
    }
    // Only when there is one to show. A 0% row next to a rate value the estimate happens to exceed
    // reads as a gain, which is the basis gap talking, not the protocol.
    if let Some(bps) = q.implied_slippage_bps.filter(|b| *b > 0) {
        fields.push(field("Discount right now", format!("{} of the rate", percent(bps))));
        if let Some(shown) = &q.quoted_display {
            fields.push(field("Rate value (no discount)", shown.clone()));
        }
    }
    fields.push(field("Your slippage", format!("{} ({slippage_bps} bps)", percent(slippage_bps))));
    // At the floor every scenario reads 90% and the list says nothing; the warning below does.
    if !q.discount_saturated {
        for s in &q.scenarios {
            fields.push(field(&format!("Discount: {}", s.label), percent(s.discount_bps)));
        }
    }
    if q.discount_saturated {
        warnings.push(format!(
            "at this size the protocol pays the floor of one tenth: about {} arrives out of the {} the rate is worth. No slippage setting changes this — the discount grows with size against the block's conversion flow, so converting a smaller amount at a time loses far less, and the Quainance route (wrap, swap, unwrap) is usually several times better. Compare them with `convert quote`.",
            q.expected_display.as_deref().unwrap_or("a tenth"),
            q.quoted_display.as_deref().unwrap_or("the quoted value"),
        ));
    } else if let Some(w) = discount_warning(q, unit, decimals) {
        warnings.push(w);
    }
    // The observation, checked against the tolerance being sent. This is the case the wallet used
    // to miss entirely: a suggestion derived from the batch model alone, below what the node's own
    // estimate already implies.
    if let Some(bps) = q.implied_slippage_bps
        && bps > slippage_bps
        && !q.discount_saturated
    {
        warnings.push(format!(
            "your slippage of {} is below the {} this conversion already loses on its own, before anyone else's conversion shares the block: it would be refunded. {} or more is the suggestion for this size.",
            percent(slippage_bps),
            percent(bps),
            percent(q.suggested_slippage_bps),
        ));
    }
    // The model, same check.
    if let Some(two) = q.scenarios.iter().find(|s| s.label.starts_with("you + one"))
        && two.discount_bps > slippage_bps
    {
        warnings.push(format!(
            "refund likely if one similar conversion shares the block: that batch discounts {}, above your {}",
            percent(two.discount_bps),
            percent(slippage_bps),
        ));
    }
    if let Some(h) = &q.hold {
        warnings.insert(0, h.note.clone());
    }
    (fields, warnings)
}

fn discount_warning(q: &ConversionQuote, unit: &str, decimals: u8) -> Option<String> {
    let spot = q.quoted.as_deref().and_then(|v| U256::from_str_radix(v, 10).ok())?;
    let expected = q.expected.as_deref().and_then(|v| U256::from_str_radix(v, 10).ok())?;
    if spot.is_zero() || expected >= spot {
        return None;
    }
    let kept_bps = expected.saturating_mul(U256::from(10_000u64)) / spot;
    let lost_pct = (10_000 - kept_bps.to::<u64>().min(10_000)) / 100;
    (lost_pct >= 20).then(|| {
        format!(
            "the protocol discount takes about {lost_pct}% of this conversion at its size: about {} {unit} arrives, not the {} the spot rate suggests; converting less at a time loses less",
            amount::format_amount_short(expected, decimals, 4),
            amount::format_amount_short(spot, decimals, 4),
        )
    })
}

/// Mailbox discovery result.
#[derive(Clone, Debug, Default, Serialize)]
pub struct MailboxSummary {
    /// Announced senders read in this pass, in announcement order.
    pub senders: Vec<String>,
    /// Those already registered as channels (rescanned in full).
    pub registered: Vec<String>,
    /// Offers to tell the user about: Qi waiting over [`OFFER_NOTICE_MIN_QITS`], not told before.
    pub new_offers: Vec<ChannelOffer>,
    /// Offers waiting for the user, all told.
    pub pending: usize,
    /// Senders whose probe found Qi but the store has no room for another channel.
    pub refused: usize,
    /// Announced senders after this pass, for a later one.
    pub deferred: usize,
    /// Announcements that are not payment codes.
    pub invalid: usize,
}

/// Result of a background payment-channel sync.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PaymentSync {
    /// Registered channels scanned (announced or not).
    pub scanned: usize,
    /// Channel offers to tell the user about (see [`MailboxSummary::new_offers`]).
    pub new_offers: Vec<ChannelOffer>,
    /// Announcements beyond the per-pass bound (picked up on later passes).
    pub deferred: usize,
    /// Channels were left unscanned because something else needed the wallet.
    pub stopped: bool,
}

/// A mailbox pass: from the cursor (rewound when due), or the whole mailbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MailboxPass {
    /// Background: new announcements, and every one again each [`MAILBOX_REPROBE_SECS`].
    Due,
    /// Asked for: every announcement, every page.
    Full,
}

/// Distinct announced senders one mailbox page reads.
pub const MAILBOX_PAGE: usize = 32;
/// Pages one background pass reads before leaving the rest to the next.
pub const MAILBOX_PAGES_PER_PASS: usize = 4;
/// How often a background pass goes back over every announcement, so a sender who announced
/// before paying is probed again.
pub const MAILBOX_REPROBE_SECS: u64 = 3600;
/// Qi waiting on an offered channel before the user is told about it: 1 Qi. Anyone can announce
/// and leave dust, so an offer below this is listed but does not notify.
pub const OFFER_NOTICE_MIN_QITS: u64 = 1_000;

/// A sender who announced a payment channel, whose probe found Qi waiting, and who is not
/// registered: nothing of theirs is in the wallet until the user accepts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelOffer {
    /// Their payment code.
    pub code: String,
    /// Qits the latest probe found (a few addresses deep: a lower bound).
    #[serde(serialize_with = "crate::ser::u256", deserialize_with = "crate::ser::de_u256")]
    pub found: U256,
    /// When the offer was first recorded, and last probed (Unix seconds).
    pub first_seen: u64,
    pub last_probe: u64,
    /// The user has been told about it.
    pub notified: bool,
}

impl ChannelOffer {
    /// The notification: who offered, how much is waiting, where to answer.
    pub fn notice(&self) -> (String, String) {
        (
            "Payment channel offered".into(),
            format!(
                "{} announced a channel with {} Qi waiting · accept or decline in People › Channels",
                crate::session::short_code(&self.code),
                crate::amount::qi(self.found)
            ),
        )
    }
}

fn offer_key(network: &str, code: &str) -> String {
    format!("mailbox_offer:{network}:{code}")
}

fn declined_key(network: &str, code: &str) -> String {
    format!("mailbox_declined:{network}:{code}")
}

/// What one announced sender came to.
#[derive(Debug, PartialEq)]
enum Announced {
    /// A registered channel.
    Channel,
    /// An offer the user should hear about now.
    Offer(ChannelOffer),
    /// Its probe found Qi but the store has no room.
    Refused,
    /// Nothing to say: no Qi found, declined, or an offer already told.
    Quiet,
}

/// Record what the mailbox said about one sender. A registered channel is marked a peer; a
/// probe that found Qi becomes (or updates) an offer, told once it reaches
/// [`OFFER_NOTICE_MIN_QITS`]; a declined sender is left alone.
fn record_announcement(
    app: &crate::appdb::AppDb,
    network: &str,
    code: &str,
    registration: ChannelRegistration,
    found: U256,
    at: u64,
) -> Result<Announced> {
    match registration {
        ChannelRegistration::Existing | ChannelRegistration::Registered => {
            app.set_kv(&format!("peer:{network}:{code}"), "1")?;
            app.delete_kv(&offer_key(network, code))?;
            Ok(Announced::Channel)
        }
        ChannelRegistration::Refused => Ok(Announced::Refused),
        ChannelRegistration::Unregistered => {
            if found.is_zero() || app.kv(&declined_key(network, code))?.is_some() {
                return Ok(Announced::Quiet);
            }
            let key = offer_key(network, code);
            let mut offer = app.kv(&key)?.and_then(|v| serde_json::from_str::<ChannelOffer>(&v).ok()).unwrap_or(ChannelOffer {
                code: code.to_string(),
                found,
                first_seen: at,
                last_probe: at,
                notified: false,
            });
            offer.found = found;
            offer.last_probe = at;
            let tell = !offer.notified && found >= U256::from(OFFER_NOTICE_MIN_QITS);
            offer.notified |= tell;
            app.set_kv(&key, &serde_json::to_string(&offer).map_err(|e| CoreError::Storage(e.to_string()))?)?;
            Ok(if tell { Announced::Offer(offer) } else { Announced::Quiet })
        }
    }
}

/// Strip control characters and bound length for untrusted display strings (token metadata).
///
/// The 32-character cap is for names and symbols. It must never be used on a value the user is
/// being asked to *check* — see [`sanitize_value`].
pub fn sanitize_display(text: &str) -> String {
    crate::explorer::clean(text, 32)
}

/// Strip control characters from a value that is about to be signed, keeping all of it.
///
/// A review exists so the user can check what they are agreeing to, and checking an address means
/// reading both ends of it. Truncating here would hide the tail — which is the half an
/// address-poisoning attacker leaves alone — behind no ellipsis at all, so the shortened value
/// would read as a complete one. Long values are the caller's problem to lay out, not this
/// function's to silently solve.
pub fn sanitize_value(text: &str) -> String {
    crate::explorer::clean(text, 4096)
}

/// Opt-in step tracing to stderr (`QUAI_TERMINAL_TRACE=1`). Never prints secrets.
pub(crate) fn trace(message: impl AsRef<str>) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ENABLED.get_or_init(|| ["QUAI_TERMINAL_TRACE", "QUAI_WALLET_TRACE"].iter().any(|name| std::env::var(name).is_ok_and(|v| v == "1")))
    {
        eprintln!(
            "[trace {:.3}] {}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0),
            message.as_ref()
        );
    }
}

/// Largest change pool that can be allocated reliably within the SDK's 100,000-attempt
/// per-pool derivation budget (Cyprus-1 Qi matches are ~1 in 500 candidates).
pub const MAX_CHANGE_POOL: usize = 48;

/// Allocate payment-code destinations one output at a time with the full search budget.
fn allocate_payment_destinations(
    store: &mut quai_sdk::wallet::storage::SqliteStore,
    payment: &quai_sdk::payments::PrivatePaymentCode,
    peer: &PaymentCode,
    qits: U256,
    count: usize,
) -> Result<Vec<QiAddress>> {
    let mut destinations = Vec::with_capacity(count);
    for _ in 0..count.min(1024) {
        destinations.extend(payment_intent(store, payment, peer, qits, 1, 100_000, || false)?.destinations);
    }
    Ok(destinations)
}

/// Per-address derivation attempts within the SDK's 100,000 total search budget.
/// A Cyprus-1 Qi match is roughly one in several hundred candidates.
fn search_budget(outputs: usize) -> u32 {
    (100_000 / outputs.max(1)).clamp(1, 20_000) as u32
}

fn no_payment() -> CoreError {
    CoreError::Locked("unlock a recovery-phrase wallet to use payment codes".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Change addresses a transaction never signed for come back. Allocating a pool, releasing
    /// it and allocating again hands out the same indexes instead of deriving fresh ones, so
    /// pools dropped by retries and rejected reviews no longer push later change past the gap a
    /// seed-only restore scans (SDK alpha.6, review finding TX-5).
    #[test]
    fn released_change_addresses_are_handed_out_again() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
        let registry = crate::registry::Registry::new(paths);
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let meta = registry.create_hd("w", phrase, "english", "", "password123", true).unwrap();
        let config = crate::config::AppConfig::default();
        let network = config.network("orchard").unwrap();
        let mut session = Session::open(registry, config, meta, network).unwrap();
        let account = session.meta.qi_account().unwrap().expect("an HD wallet has a Qi account");
        let start = session.change_cursor(&account).unwrap();
        let pool = session.change_pool(2).unwrap();
        let after = session.change_cursor(&account).unwrap();
        assert!(after > start, "two change addresses were derived");
        session.release_pool(pool);
        let pool = session.change_pool(2).unwrap();
        assert_eq!(session.change_cursor(&account).unwrap(), after, "the same two were handed out again");
        // A pool that is dropped without being released still burns its addresses.
        drop(pool);
        let _ = session.change_pool(2).unwrap();
        assert!(session.change_cursor(&account).unwrap() > after, "dropping a pool without releasing it burns it");
    }

    /// A bare quote to build cases on; every conversion test below sets only what it is about.
    fn quote_fixture(quoted: &str, expected: &str) -> ConversionQuote {
        ConversionQuote {
            direction: "quai_to_qi".into(),
            amount: String::new(),
            amount_display: String::new(),
            quoted: Some(quoted.into()),
            quoted_display: None,
            expected: Some(expected.into()),
            expected_display: None,
            implied_slippage_bps: None,
            discount_saturated: false,
            hold: None,
            headline: String::new(),
            flow_amount: None,
            scenarios: vec![],
            suggested_slippage_bps: 0,
            minimum: None,
            notes: vec![],
            explorer_steps: None,
        }
    }

    fn scenario(label: &str, bps: u16) -> RiskScenario {
        RiskScenario { label: label.into(), batch_quai: String::new(), discount_bps: bps }
    }

    /// A conversion that loses a fifth or more to the discount says so, with both figures; one
    /// that keeps most of its value does not.
    #[test]
    fn a_large_conversion_discount_is_warned() {
        let w = discount_warning(&quote_fixture("75964", "7582"), "Qi", 3).unwrap();
        assert!(w.contains("about 90%") && w.contains("7.582") && w.contains("75.964"), "{w}");
        assert_eq!(discount_warning(&quote_fixture("759", "756"), "Qi", 3), None, "0.4% is not worth a warning");
    }

    /// The suggestion has to clear both figures the node offers. Measured on mainnet 2026-09-20:
    /// at 50 QUAI the batch model says 20 bps while the node's own estimate already implies 75, and
    /// the wallet used to recommend the model alone — 70 bps into a conversion that would be
    /// refunded. Taking the larger can only raise the tolerance, so it can never cause a refund.
    #[test]
    fn the_suggestion_clears_the_model_and_the_observation() {
        let suggest =
            |model: u16, implied: Option<u16>| model.max(implied.unwrap_or(0)).saturating_add(SLIPPAGE_MARGIN_BPS).clamp(30, 9000);
        assert_eq!(suggest(20, Some(75)), 125, "the observation wins at 50 QUAI, where the model alone gave 70");
        assert_eq!(suggest(1260, Some(266)), 1310, "the model wins at 250 QUAI: a shared block is the real risk");
        assert_eq!(suggest(20, None), 70, "a node that cannot price the discount falls back to the model");
        assert_eq!(suggest(9000, Some(9007)), 9000, "never above the range ConversionSlippage accepts");
        assert_eq!(suggest(0, Some(0)), 50, "and never below the floor the node clamps to");
    }

    /// The review checks the tolerance against both figures, and says which one it failed.
    #[test]
    fn a_tolerance_below_either_figure_is_warned() {
        let mut q = quote_fixture("1000", "990");
        q.implied_slippage_bps = Some(75);
        q.suggested_slippage_bps = 125;
        q.scenarios = vec![scenario("your conversion alone", 20), scenario("you + one similar conversion", 20)];
        let (_, warnings) = conversion_review_context(&q, 70, "Qi", 3);
        let observed = warnings.iter().find(|w| w.contains("on its own")).expect("the observation is checked");
        assert!(observed.contains("0.70%") && observed.contains("0.75%") && observed.contains("1.25%"), "{observed}");
        // Above both, nothing to say.
        let (_, quiet) = conversion_review_context(&q, 200, "Qi", 3);
        assert!(!quiet.iter().any(|w| w.contains("refunded") || w.contains("refund likely")), "{quiet:?}");
        // The batch model is checked too, even when the observation is fine.
        q.scenarios = vec![scenario("your conversion alone", 20), scenario("you + one similar conversion", 1260)];
        let (_, model) = conversion_review_context(&q, 200, "Qi", 3);
        assert!(model.iter().any(|w| w.contains("refund likely") && w.contains("12.60%")), "{model:?}");
    }

    /// At the floor the scenario list says 90% four times and answers nothing. Drop it, and say the
    /// one thing that is true: no tolerance changes this, so convert less or take the other market.
    #[test]
    fn a_saturated_discount_replaces_the_scenarios_with_the_point() {
        let mut q = quote_fixture("1235820000000000000000", "124350000000000000000");
        q.discount_saturated = true;
        q.implied_slippage_bps = Some(8994);
        q.expected_display = Some("124.35 QUAI".into());
        q.quoted_display = Some("1,235.82 QUAI".into());
        q.scenarios = vec![scenario("your conversion alone", 9000), scenario("you + one similar conversion", 9000)];
        let (fields, warnings) = conversion_review_context(&q, 9000, "QUAI", 18);
        assert!(!fields.iter().any(|f| f.label.starts_with("Discount:")), "the 90% list is not shown");
        let w = warnings.first().expect("the floor is the first thing said");
        assert!(w.contains("one tenth") && w.contains("124.35 QUAI") && w.contains("Quainance"), "{w}");
        assert!(!warnings.iter().any(|w| w.contains("on its own")), "a tolerance warning here would imply one exists");
    }

    /// Mainnet is past both windows; Orchard is below the second. The direction decides the cost.
    #[test]
    fn a_hold_window_is_recognised_and_priced_by_direction() {
        assert!(conversion_hold("quai_to_qi", 2_257_268).is_none(), "mainnet is past both windows");
        assert!(conversion_hold("quai_to_qi", 1_729_191).is_none(), "orchard has not reached the second yet");
        let burning = conversion_hold("quai_to_qi", 1_755_000).expect("the window opens at the fork block itself");
        assert_eq!((burning.until_prime, burning.blocks_remaining, burning.burns_gas), (1_775_000, 20_000, true));
        assert!(burning.note.contains("gas is burned"), "{}", burning.note);
        let free = conversion_hold("qi_to_quai", 1_774_999).expect("still held one block before the end");
        assert_eq!((free.until_prime, free.blocks_remaining, free.burns_gas), (1_775_000, 1, false));
        assert!(free.note.contains("nothing is spent"), "{}", free.note);
        assert!(conversion_hold("qi_to_quai", 1_775_000).is_none(), "and free again at the end");
        // The earlier window is modelled too, with the same shape.
        assert!(conversion_hold("quai_to_qi", 1_171_500).is_some_and(|h| h.until_prime == 1_191_500));
    }

    /// The one sentence every surface leads with.
    #[test]
    fn the_headline_says_in_out_and_cost() {
        let h = conversion_headline("1,000 QUAI", Some("0.804 Qi"), Some("8.091 Qi"), Some(9007));
        assert_eq!(h, "1,000 QUAI → about 0.804 Qi, which is 90.07% below the rate right now");
        // Zero is "too small to see", never "free": the cubic discount has a 20 bps floor, and what
        // hides it is the basis gap between the two quotes.
        let none = conversion_headline("10 QUAI", Some("0.080 Qi"), Some("0.080 Qi"), Some(0));
        assert!(none.ends_with("the discount is too small to measure against the rate"), "{none}");
        assert!(!none.contains("no discount"), "{none}");
        let unpriced = conversion_headline("5 Qi", None, Some("617 QUAI"), None);
        assert!(unpriced.contains("did not price the discount"), "{unpriced}");
        assert_eq!(conversion_headline("5 Qi", None, None, None), "5 Qi → no quote available");
    }

    /// An announced sender is never registered on its own: Qi found becomes an offer, told once
    /// it reaches the threshold; dust and declined senders stay quiet; a registered channel
    /// clears its offer.
    #[test]
    fn announcements_become_offers_not_channels() {
        let app = crate::appdb::AppDb::memory().unwrap();
        let net = "mainnet";
        let unregistered = ChannelRegistration::Unregistered;
        assert_eq!(record_announcement(&app, net, "PMnobody", unregistered, U256::ZERO, 10).unwrap(), Announced::Quiet, "nothing waiting");
        assert!(app.kv(&offer_key(net, "PMnobody")).unwrap().is_none(), "and nothing recorded");
        // Dust: listed, not told.
        assert_eq!(record_announcement(&app, net, "PMdust", unregistered, U256::from(5u64), 10).unwrap(), Announced::Quiet);
        assert!(app.kv(&offer_key(net, "PMdust")).unwrap().is_some());
        // Real Qi: told once, and the offer follows later probes.
        let Announced::Offer(offer) = record_announcement(&app, net, "PMalice", unregistered, U256::from(2_500u64), 10).unwrap() else {
            panic!("expected an offer")
        };
        assert_eq!((offer.found, offer.first_seen, offer.notified), (U256::from(2_500u64), 10, true));
        assert_eq!(
            record_announcement(&app, net, "PMalice", unregistered, U256::from(4_000u64), 20).unwrap(),
            Announced::Quiet,
            "told once"
        );
        let stored: ChannelOffer = serde_json::from_str(&app.kv(&offer_key(net, "PMalice")).unwrap().unwrap()).unwrap();
        assert_eq!((stored.found, stored.first_seen, stored.last_probe), (U256::from(4_000u64), 10, 20));
        // Dust that grows past the threshold is told then.
        assert!(matches!(record_announcement(&app, net, "PMdust", unregistered, U256::from(1_000u64), 30).unwrap(), Announced::Offer(_)));
        // Declined: never offered again.
        app.set_kv(&declined_key(net, "PMmallory"), "1").unwrap();
        assert_eq!(record_announcement(&app, net, "PMmallory", unregistered, U256::from(9_000u64), 40).unwrap(), Announced::Quiet);
        assert!(app.kv(&offer_key(net, "PMmallory")).unwrap().is_none());
        // Registered (accepted, or added by hand): a channel, and the offer is gone.
        assert_eq!(record_announcement(&app, net, "PMalice", ChannelRegistration::Existing, U256::ZERO, 50).unwrap(), Announced::Channel);
        assert!(app.kv(&offer_key(net, "PMalice")).unwrap().is_none());
        assert_eq!(app.kv("peer:mainnet:PMalice").unwrap().as_deref(), Some("1"));
        assert_eq!(
            record_announcement(&app, net, "PMfull", ChannelRegistration::Refused, U256::from(9u64), 60).unwrap(),
            Announced::Refused
        );
        // Offers are per network.
        assert_eq!(app.kv_prefix(&offer_key("orchard", "")).unwrap().len(), 0);
        assert_eq!(app.kv_prefix(&offer_key(net, "")).unwrap().len(), 1, "only the dust one is left");
    }

    #[test]
    fn greedy_denominations() {
        assert_eq!(denomination_count(U256::from(1000u64)), 1);
        assert_eq!(denomination_count(U256::from(1500u64)), 2);
        assert_eq!(denomination_count(U256::from(250u64)), 3);
        assert_eq!(denomination_count(U256::from(3u64)), 3);
    }

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_display("WQ\u{1b}[31mI\u{202E}"), "WQ[31mI");
    }
}

// --------------------------------------------------- arbitrary contracts

/// What a read-only contract function answered.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallResult {
    /// `balanceOf(address)`.
    pub signature: String,
    /// One line per returned value, named where the ABI names it.
    pub outputs: Vec<(String, String)>,
}

impl Session {
    /// What this address is, for the send form: a plain account, or a contract and what its own
    /// metadata says it offers. Always [`Trust::FirstHand`] — the answer decides what the form
    /// lets someone do, so it is never served from a memo.
    pub async fn inspect_contract(&self, to: &str) -> Result<crate::contracts::Discovered> {
        let recipient = match self.resolve_recipient(to)? {
            Recipient::Quai(a) => a,
            Recipient::Qi(_) | Recipient::PaymentCode(_) => {
                return Err(CoreError::Invalid("a Qi destination has no contract code".into()));
            }
        };
        self.data_ctx_at(Trust::FirstHand)?.discover_contract(&recipient.to_string(), Trust::FirstHand).await
    }

    /// Call a read-only function and decode what it returned. Nothing is signed and nothing is
    /// sent; this is a node simulation, so it costs nothing and cannot move anything.
    pub async fn call_contract_read(&self, to: &str, signature: &str, args: &[String]) -> Result<CallResult> {
        let (found, interface, callable) = self.callable(to, signature).await?;
        let address = crate::chain::addr(&found.address)?;
        if !callable.read_only {
            return Err(CoreError::Invalid(format!("{} changes state; it has to be signed, not read", callable.name)));
        }
        let values = encode_arguments(&callable, args)?;
        let caller = crate::chain::addr(&self.account(None)?.address)?;
        let contract = quai_sdk::contracts::Contract::new(address, interface, &self.node.provider);
        let out = contract
            .call(caller, &callable.signature, &values, BlockTag::Latest)
            .await
            .map_err(|e| CoreError::Network(format!("{}: {e}", callable.name)))?;
        let function = contract.interface().function(&callable.signature).map_err(|e| CoreError::Invalid(e.to_string()))?;
        let names = function.output_names().to_vec();
        Ok(CallResult {
            signature: callable.signature.clone(),
            outputs: out
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let name = names.get(i).filter(|n| !n.is_empty()).cloned().unwrap_or_else(|| format!("out{i}"));
                    (name, crate::ops::sanitize_value(&value_text(v)))
                })
                .collect(),
        })
    }

    /// Review a call to a contract the wallet was never taught about, described by the ABI its own
    /// bytecode names.
    ///
    /// The ABI decides how the arguments are encoded and how the review reads them back, so the
    /// review states plainly where that ABI came from and what it is worth. The raw calldata is a
    /// field of its own: it is the only part of this that is certainly true, and it is what
    /// actually gets signed.
    pub async fn review_contract_call(
        &mut self,
        account: Option<&str>,
        to: &str,
        signature: &str,
        args: &[String],
        value: Option<&str>,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        let from = self.account(account)?;
        // One inspection, not two: the ABI that encodes the call must be the same one the review
        // describes. Reading twice would also re-open the window `Trust::FirstHand` exists to
        // close — code can be replaced between two reads.
        let (found, interface, callable) = self.callable(to, signature).await?;
        let address = crate::chain::addr(&found.address)?;
        if callable.read_only {
            return Err(CoreError::Invalid(format!("{} only reads; nothing to sign", callable.name)));
        }
        let quai = match value.map(str::trim).filter(|v| !v.is_empty() && *v != "0") {
            Some(v) if !callable.payable => {
                return Err(CoreError::Invalid(format!("{} is not payable; it cannot carry {v} QUAI", callable.name)));
            }
            Some(v) => amount::parse_quai(v)?,
            None => U256::ZERO,
        };
        let values = encode_arguments(&callable, args)?;
        let contract = quai_sdk::contracts::Contract::new(address, interface, &self.node.provider);
        let call = contract.prepare(&callable.signature, &values, quai).map_err(|e| CoreError::Invalid(e.to_string()))?;

        // Simulate before it is offered for signing: a call that already reverts should be a
        // message here, not a burnt fee. A node that cannot answer is not a reason to block.
        let caller = crate::chain::addr(&from.address)?;
        let mut warnings = Vec::new();
        match contract.simulate(caller, &call, BlockTag::Latest, None).await {
            Ok(_) => {}
            Err(e) => warnings.push(format!("this call fails when simulated now: {e}")),
        }

        // What the ABI is worth, said in the review rather than assumed by it.
        warnings.push(match found.trust_note() {
            Some(note) => format!("arguments shown are decoded with {note}"),
            None => "this contract publishes no ABI; the call data below is all there is".into(),
        });
        if found.verified != Some(true) {
            warnings.push("the explorer has not verified this contract against its source".into());
        }
        warnings.extend(self.recipient_warnings(&address.to_string()));

        let call_data = format!("0x{}", hex::encode(call.data().bytes()));
        let mut fields = vec![
            field("Contract", found.metadata.as_ref().map(|m| format!("{} · {}", m.name, address)).unwrap_or_else(|| address.to_string())),
            field("Function", callable.signature.clone()),
        ];
        for ((name, ty), typed) in callable.inputs.iter().zip(args) {
            let label = if name.is_empty() { ty.clone() } else { format!("{name} ({ty})") };
            fields.push(field(&label, crate::ops::sanitize_value(typed)));
        }
        fields.push(field("Call data", call_data.clone()));

        let intent = crate::data::with_access_list(&self.node.provider, caller, call).await?.into_account_intent();
        self.prepare_account(AccountRequest {
            from,
            intent,
            kind: "contract_call".into(),
            title: format!("Call {}", callable.name),
            asset: "QUAI".into(),
            amount: quai,
            decimals: QUAI_DECIMALS,
            counterparty: address.to_string(),
            fields,
            warnings,
            detail: serde_json::json!({
                "contract": address.to_string(),
                "function": callable.signature,
                "data": call_data,
                "abi_source": found.metadata.as_ref().map(|m| m.cid.clone()),
                "verified": found.verified,
                "undeclared": found.undeclared,
            }),
            max_gas: 1_000_000,
            max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
        })
        .await
    }

    /// Resolve a destination and one of its functions, by signature or by an unambiguous name.
    async fn callable(
        &self,
        to: &str,
        signature: &str,
    ) -> Result<(crate::contracts::Discovered, quai_sdk::abi::AbiInterface, crate::contracts::Callable)> {
        let found = self.inspect_contract(to).await?;
        if !found.is_contract() {
            return Err(CoreError::Invalid(format!("{to} is a plain account, not a contract")));
        }
        let metadata = found
            .metadata
            .as_ref()
            .ok_or_else(|| CoreError::Invalid(found.metadata_error.clone().unwrap_or_else(|| "this contract publishes no ABI".into())))?;
        let interface = metadata.interface()?;
        let wanted = signature.trim();
        let list = crate::contracts::callables(&interface);
        let matches: Vec<&crate::contracts::Callable> = list.iter().filter(|c| c.signature == wanted || c.name == wanted).collect();
        let callable = match matches.as_slice() {
            [one] => (*one).clone(),
            [] => return Err(CoreError::Invalid(format!("`{wanted}` is not a function this contract declares"))),
            several => {
                let names: Vec<&str> = several.iter().map(|c| c.signature.as_str()).collect();
                return Err(CoreError::Invalid(format!("`{wanted}` is overloaded; name one of: {}", names.join(", "))));
            }
        };
        Ok((found, interface, callable))
    }
}

/// Parse each typed argument against the type the ABI declares for it.
fn encode_arguments(callable: &crate::contracts::Callable, args: &[String]) -> Result<Vec<serde_json::Value>> {
    if args.len() != callable.inputs.len() {
        return Err(CoreError::Invalid(format!("{} takes {} argument(s), got {}", callable.name, callable.inputs.len(), args.len())));
    }
    callable
        .inputs
        .iter()
        .zip(args)
        .map(|((name, ty), text)| {
            let what = if name.is_empty() { ty.clone() } else { name.clone() };
            let named = |e: CoreError| CoreError::Invalid(format!("{what}: {e}"));
            let value = crate::contracts::parse_argument(ty, text).map_err(named)?;
            // Then the coder itself, on the exact value that will be encoded into the calldata.
            //
            // This is what makes the parsing above safe to be incomplete: it may refuse more than
            // the coder would, never less, because whatever it lets through still has to survive
            // this. Without it, a gap in the parser would be a value encoded differently from what
            // was typed; with it, the worst case is a duller error message.
            let parsed_type = quai_sdk::abi::AbiType::parse(ty).map_err(|e| named(CoreError::Invalid(e.to_string())))?;
            quai_sdk::abi::AbiCoder::encode(std::slice::from_ref(&parsed_type), std::slice::from_ref(&value))
                .map_err(|e| named(CoreError::Invalid(format!("{e} (expects {ty})"))))?;
            Ok(value)
        })
        .collect()
}

/// A decoded ABI value as one line of text.
fn value_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod argument_tests {
    use super::*;
    use crate::contracts::Callable;

    fn takes(ty: &str) -> Callable {
        Callable {
            signature: format!("f({ty})"),
            name: "f".into(),
            inputs: vec![("x".into(), ty.into())],
            payable: false,
            read_only: false,
        }
    }

    /// Whatever the wallet's own parsing lets through still has to encode against the declared
    /// type. That is what makes the parsing safe to be incomplete: it may refuse more than the
    /// coder would, never less. A tuple is the case it does not check itself.
    #[test]
    fn the_coder_has_the_last_word_on_every_argument() {
        // A tuple the parser waves through because it cannot split the type: the coder refuses it,
        // and the error still names the argument rather than only the type.
        let wrong_arity = encode_arguments(&takes("(address,uint256)"), &["[\"0x0000000000000000000000000000000000000001\"]".into()])
            .unwrap_err()
            .to_string();
        assert!(wrong_arity.contains("x:"), "the argument is named: {wrong_arity}");
        assert!(wrong_arity.contains("(address,uint256)"), "and the type it wanted: {wrong_arity}");
        // A well-formed tuple goes through.
        assert!(encode_arguments(&takes("(address,uint256)"), &["[\"0x0000000000000000000000000000000000000001\", \"7\"]".into()]).is_ok());
        // An element the parser already refuses is named by position, before the coder is reached.
        let element = encode_arguments(&takes("uint256[]"), &["[\"1\", \"nope\"]".into()]).unwrap_err().to_string();
        assert!(element.contains("item 1"), "{element}");
        // Arity is still checked against the ABI, not just per argument.
        let count = encode_arguments(&takes("uint256"), &[]).unwrap_err().to_string();
        assert!(count.contains("takes 1 argument"), "{count}");
        // A type the wallet cannot fill in is refused rather than guessed at.
        assert!(encode_arguments(&takes("function"), &["x".into()]).is_err());
    }
}

/// Public conversion preview for the independent data lane. Placeholder addresses are used only
/// for ledger/zone rate observations when no owner was supplied; this function never prepares.
pub async fn quote_conversion(
    ctx: &crate::data::DataCtx,
    direction: &str,
    value: &str,
    quai_address: Option<&str>,
    qi_address: Option<&str>,
) -> Result<ConversionQuote> {
    let header = ctx.node.provider.latest_header(ZONE).await?.ok_or_else(|| CoreError::Network("no header".into()))?;
    let flow = header
        .extensions
        .fields()
        .get("conversionFlowAmount")
        .and_then(|v| v.as_str())
        .and_then(|s| U256::from_str_radix(s.trim_start_matches("0x"), 16).ok());
    let (amount_base, amount_display, minimum) = match direction {
        "quai_to_qi" => {
            let its = amount::parse_quai(value)?;
            let floor = U256::from(quai_sdk::consensus::MIN_QUAI_CONVERSION_VALUE);
            (its, format!("{} QUAI", amount::quai(its)), Some(format!("{} QUAI", amount::quai(floor))))
        }
        "qi_to_quai" => {
            let qits = amount::parse_qi(value)?;
            (qits, format!("{} Qi", amount::qi(qits)), None)
        }
        _ => return Err(CoreError::Invalid("direction must be quai_to_qi or qi_to_quai".into())),
    };
    // The spot rate ignores the controller's flow discounts; ask the node for the real estimate.
    // The estimate does not depend on the addresses, only their ledgers and zone; wallets
    // without one of each (watch-only, Quai-only) use codeless Cyprus-1 placeholders so the
    // headline is never the undiscounted spot value.
    let quai_addr = quai_address
        .and_then(|a| parse_any_address(a).ok())
        .or_else(|| parse_any_address("0x0000000000000000000000000000000000000001").ok());
    let qi_addr =
        qi_address.and_then(|a| parse_any_address(a).ok()).or_else(|| parse_any_address("0x0080000000000000000000000000000000000001").ok());
    // One call gives the undiscounted rate, the node's discounted estimate and the gap between
    // them. Asking for the halves separately is what let the wallet recommend a tolerance it had
    // never compared against the live quote.
    let estimate = match (direction, quai_addr, qi_addr) {
        ("quai_to_qi", Some(from), Some(to)) => ctx.node.provider.estimate_conversion(from, to, amount_base).await.ok(),
        ("qi_to_quai", Some(to), Some(from)) => ctx.node.provider.estimate_conversion(from, to, amount_base).await.ok(),
        _ => None,
    };
    // A node that prices the rate but not the conversion still gives a useful headline, so fall
    // back to the bare rate rather than reporting nothing.
    let quoted = match &estimate {
        Some(e) => Some(e.rate_amount),
        None if direction == "quai_to_qi" => ctx.node.provider.quai_to_qi(ZONE, amount_base, BlockTag::Latest).await?,
        None => ctx.node.provider.qi_to_quai(ZONE, amount_base, BlockTag::Latest).await?,
    };
    let expected = estimate.map(|e| e.expected_amount);
    let implied_slippage_bps = estimate.map(|e| e.implied_slippage_bps);
    // Display only, so cut the tail: eighteen decimals of QUAI in a headline is noise, and Qi
    // carries three to begin with.
    let dest = |v: U256| {
        if direction == "quai_to_qi" {
            format!("{} Qi", amount::format_amount_short(v, QI_DECIMALS, 3))
        } else {
            format!("{} QUAI", amount::format_amount_short(v, QUAI_DECIMALS, 4))
        }
    };
    let quoted_display = quoted.map(dest);
    let expected_display = expected.map(dest);
    // Qi-to-Quai shares a batch measured in QUAI, so its batch weight is what it pays out, not
    // what it spends.
    let batch_its = if direction == "quai_to_qi" { amount_base } else { quoted.unwrap_or(U256::ZERO) };
    let mut scenarios = Vec::new();
    let mut notes = vec![
            "Conversions in the same prime block share one discount; a conversion whose discount exceeds its slippage is refunded (you lose only the fee).".into(),
            "Other users' conversions change the outcome; these are estimates, not guarantees.".into(),
        ];
    let mut suggested = 100u16;
    if let Some(flow) = flow {
        let add = |label: &str, total: U256, s: &mut Vec<RiskScenario>| {
            if let Some(bps) = quai_sdk::consensus::conversion_batch_discount_bps(total, flow) {
                s.push(RiskScenario {
                    label: label.into(),
                    batch_quai: amount::format_amount_short(total, QUAI_DECIMALS, 2),
                    discount_bps: bps,
                });
            }
        };
        add("your conversion alone", batch_its, &mut scenarios);
        add("you + one similar conversion", batch_its.saturating_mul(U256::from(2)), &mut scenarios);
        add(
            "you + ~270 QUAI of other flow (observed mainnet)",
            batch_its.saturating_add(U256::from(270u128) * U256::from(10u128.pow(18))),
            &mut scenarios,
        );
        add("batch at 2x block flow", flow.saturating_mul(U256::from(2)), &mut scenarios);
        if let Some(two) = scenarios.get(1) {
            suggested = two.discount_bps;
        }
    } else {
        notes.push("This node does not report the block conversion flow; concurrent-flow risk is unknown.".into());
    }
    // Two different numbers, and the tolerance has to clear both. The batch model says what a
    // block shared with one similar conversion would cost; the node's own estimate says what
    // this conversion loses right now, alone. Taking the larger can only ever raise the
    // tolerance, so it cannot introduce a refund, and it stops the wallet recommending 70 bps
    // into an observed 75. See docs/SDK_UPDATE_REVIEW_2026-09-20.md.
    let suggested = suggested.max(implied_slippage_bps.unwrap_or(0)).saturating_add(SLIPPAGE_MARGIN_BPS).clamp(30, 9000);
    // Past the point where the cubic discount saturates, the node pays a tenth whatever
    // tolerance is sent: 9000 is not a setting that avoids the loss, it is the loss. Flagged so
    // the surfaces can stop presenting it as a choice.
    let discount_saturated =
        implied_slippage_bps.is_some_and(|b| b >= SATURATED_BPS) || scenarios.first().is_some_and(|s| s.discount_bps >= SATURATED_BPS);
    let mut explorer_steps = None;
    if ctx.policy.market
        && let Ok(steps) = ctx.explorer.convert_steps(direction, &amount_base.to_string()).await
    {
        explorer_steps = Some(steps);
    }
    // Only when the headline had to say the discount is unmeasurable, so it is not read as free.
    if implied_slippage_bps == Some(0) {
        notes.push(
                "The discount is never nil — it is at least 0.20% — but at this size it is smaller than the gap between the node's two quotes, which are read against different blocks.".into(),
            );
    }
    let hold = conversion_hold(direction, header.prime_terminus_number);
    if let Some(h) = &hold {
        notes.push(h.note.clone());
    }
    Ok(ConversionQuote {
        headline: conversion_headline(&amount_display, expected_display.as_deref(), quoted_display.as_deref(), implied_slippage_bps),
        direction: direction.into(),
        amount: amount_base.to_string(),
        amount_display,
        quoted: quoted.map(|q| q.to_string()),
        quoted_display,
        expected: expected.map(|v| v.to_string()),
        expected_display,
        implied_slippage_bps,
        discount_saturated,
        hold,
        flow_amount: flow.map(|f| f.to_string()),
        scenarios,
        suggested_slippage_bps: suggested,
        minimum,
        notes,
        explorer_steps,
    })
}
