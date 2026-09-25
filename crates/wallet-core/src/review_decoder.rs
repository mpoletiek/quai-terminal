//! The review decoder: what a transaction's own bytes do, checked against what its review says.
//!
//! A review is built by the same code that builds the calldata, so a bug there — or a feed that
//! handed the builder a wrong address or amount — would show the user a review that agrees with
//! the wrong transaction. This module does not trust the builder. It takes the exact unsigned
//! transaction (destination, value, calldata), decodes it against its own declarations of every
//! function the wallet calls, and derives the transaction's **economics**: what can leave the
//! wallet, what allowances it grants, who it pays, what it promises at least, and its deadline.
//! Those must fit inside what the review declared — its financial effects, amount, counterparty
//! and detail — or the review is refused before it can be approved.
//!
//! A call to a contract the wallet was never taught about decodes as [`Call::Unknown`]. It is
//! allowed only as a `contract_call`, and only behind a raw-call review with a typed
//! confirmation ([`Risk`]).

use crate::journal::{Detail, OpKind};
use quai_sdk::U256;
use std::sync::OnceLock;

/// Every function the wallet itself calls, declared here independently of the builders.
const DECLARATIONS: &[&str] = &[
    // ERC-20 / ERC-721 / ERC-1155
    "function transfer(address to, uint256 amount)",
    "function approve(address spender, uint256 amount)",
    "function setApprovalForAll(address operator, bool approved)",
    "function safeTransferFrom(address from, address to, uint256 tokenId)",
    "function safeTransferFrom(address from, address to, uint256 id, uint256 amount, bytes data)",
    // UniswapV2 Router02 (every venue the wallet trades on)
    "function swapExactTokensForTokens(uint256 amountIn, uint256 amountOutMin, address[] path, address to, uint256 deadline)",
    "function swapExactETHForTokens(uint256 amountOutMin, address[] path, address to, uint256 deadline)",
    "function swapExactTokensForETH(uint256 amountIn, uint256 amountOutMin, address[] path, address to, uint256 deadline)",
    "function swapExactTokensForTokensSupportingFeeOnTransferTokens(uint256 amountIn, uint256 amountOutMin, address[] path, address to, uint256 deadline)",
    "function swapExactETHForTokensSupportingFeeOnTransferTokens(uint256 amountOutMin, address[] path, address to, uint256 deadline)",
    "function swapExactTokensForETHSupportingFeeOnTransferTokens(uint256 amountIn, uint256 amountOutMin, address[] path, address to, uint256 deadline)",
    "function swapTokensForExactTokens(uint256 amountOut, uint256 amountInMax, address[] path, address to, uint256 deadline)",
    "function swapETHForExactTokens(uint256 amountOut, address[] path, address to, uint256 deadline)",
    "function swapTokensForExactETH(uint256 amountOut, uint256 amountInMax, address[] path, address to, uint256 deadline)",
    "function addLiquidity(address tokenA, address tokenB, uint256 amountADesired, uint256 amountBDesired, uint256 amountAMin, uint256 amountBMin, address to, uint256 deadline)",
    "function addLiquidityETH(address token, uint256 amountTokenDesired, uint256 amountTokenMin, uint256 amountETHMin, address to, uint256 deadline)",
    "function removeLiquidity(address tokenA, address tokenB, uint256 liquidity, uint256 amountAMin, uint256 amountBMin, address to, uint256 deadline)",
    "function removeLiquidityETH(address token, uint256 liquidity, uint256 amountTokenMin, uint256 amountETHMin, address to, uint256 deadline)",
    // WQUAI and WQI
    "function deposit()",
    "function withdraw(uint256 amount)",
    "function claimDeposit()",
    "function unwrapQi(address beneficiary, uint256 amount, uint64 etxGas)",
    // Quainance bonding curves
    "function buy(uint256 minimumTokenAmount, uint256 deadline)",
    "function sell(uint256 maximumTokenAmount, uint256 minimumQuoteAmount, uint256 deadline)",
    "function claimQuote(address recipient)",
    // HartiiLabs curves
    "function buy(uint256 minTokensOut)",
    "function sell(uint256 tokensIn, uint256 minQuaiOut)",
    // Gauges (core and launch zone share these calls)
    "function stake(uint256 pid, uint256 amount)",
    "function withdraw(uint256 pid, uint256 amount)",
    "function getReward(uint256 pid, address[] tokens)",
    "function exit(uint256 pid, address[] tokens)",
    "function notifyRewardAmount(uint256 pid, address rewardToken, uint256 amount, uint256 duration)",
    // Zora V3 asks and its module manager
    "function fillAsk(address tokenContract, uint256 tokenId, address fillCurrency, uint256 fillAmount, address finder)",
    "function createAsk(address tokenContract, uint256 tokenId, uint256 askPrice, address askCurrency, address sellerFundsRecipient, uint16 findersFeeBps)",
    "function setAskPrice(address tokenContract, uint256 tokenId, uint256 askPrice, address askCurrency)",
    "function cancelAsk(address tokenContract, uint256 tokenId)",
    "function setApprovalForModule(address module, bool approved)",
    // The message board
    "function post(bytes32 tag, uint8 kind, bytes body)",
];

fn interface() -> &'static quai_sdk::abi::AbiInterface {
    static ABI: OnceLock<quai_sdk::abi::AbiInterface> = OnceLock::new();
    ABI.get_or_init(|| quai_sdk::abi::AbiInterface::from_human_readable(DECLARATIONS).expect("review decoder declarations parse"))
}

/// What a transaction's bytes call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    /// No calldata: a plain transfer of the native value.
    NativeTransfer,
    /// The two-byte slippage of a QUAI → Qi conversion (sent to a Qi address).
    Conversion { slippage_bps: u16 },
    Erc20Transfer { to: String, amount: U256 },
    Approve { spender: String, amount: U256 },
    ApproveAll { operator: String, approved: bool },
    ModuleApproval { module: String, approved: bool },
    /// A router swap. `amount_in` is `None` when it spends the native value; `min_out` is the
    /// exact output for an exact-output swap.
    Swap { exact_output: bool, amount_in: Option<U256>, min_out: U256, path: Vec<String>, native_out: bool, to: String, deadline: U256 },
    AddLiquidity { token_a: String, token_b: Option<String>, amount_a: U256, amount_b: Option<U256>, min_a: U256, min_b: U256, to: String, deadline: U256 },
    RemoveLiquidity { token_a: String, token_b: Option<String>, liquidity: U256, min_a: U256, min_b: U256, to: String, deadline: U256 },
    WrapQuai,
    UnwrapQuai { amount: U256 },
    ClaimWqi,
    UnwrapWqi { beneficiary: String, amount: U256 },
    CurveBuy { min_tokens: U256, deadline: Option<U256> },
    CurveSell { max_tokens: U256, min_quote: U256, deadline: Option<U256> },
    CurveClaim { recipient: String },
    GaugeStake { pid: U256, amount: U256 },
    GaugeWithdraw { pid: U256, amount: U256 },
    GaugeReward { pid: U256, exit: bool },
    GaugeNotify { pid: U256, token: String, amount: U256, duration: U256 },
    ZoraFill { contract: String, token_id: U256, currency: String, amount: U256 },
    ZoraCreate { contract: String, token_id: U256, price: U256, currency: String, funds_recipient: String },
    ZoraSetPrice { contract: String, token_id: U256, price: U256, currency: String },
    ZoraCancel { contract: String, token_id: U256 },
    NftTransfer { from: String, to: String, token_id: U256, amount: Option<U256> },
    BoardPost { body_len: usize },
    /// A selector none of the declarations match.
    Unknown { selector: [u8; 4] },
}

/// A decoded transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    /// Destination, lowercase (`None`: contract creation).
    pub to: Option<String>,
    /// Native value.
    pub value: U256,
    pub call: Call,
}

fn uint(v: &serde_json::Value) -> Option<U256> {
    match v {
        serde_json::Value::String(s) => {
            if let Some(hex) = s.strip_prefix("0x") {
                U256::from_str_radix(hex, 16).ok()
            } else {
                U256::from_str_radix(s, 10).ok()
            }
        }
        serde_json::Value::Number(n) => n.as_u64().map(U256::from),
        _ => None,
    }
}

fn address(v: &serde_json::Value) -> Option<String> {
    v.as_str().filter(|s| s.starts_with("0x") && s.len() == 42).map(str::to_lowercase)
}

fn addresses(v: &serde_json::Value) -> Option<Vec<String>> {
    v.as_array()?.iter().map(address).collect()
}

const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

/// Decode a transaction. `Err` is calldata that names a declared function but does not decode
/// canonically under it (never guessed at); an unknown selector is `Ok(Call::Unknown)`.
pub fn decode(to: Option<&str>, value: U256, data: &[u8]) -> Result<Decoded, String> {
    let to = to.map(str::to_lowercase);
    let call = decode_call(to.as_deref(), data)?;
    Ok(Decoded { to, value, call })
}

fn decode_call(to: Option<&str>, data: &[u8]) -> Result<Call, String> {
    if data.is_empty() {
        return Ok(Call::NativeTransfer);
    }
    // A conversion is sent to a Qi address with exactly two bytes of slippage.
    if data.len() == 2 && to.is_some_and(is_qi_address) {
        return Ok(Call::Conversion { slippage_bps: u16::from_be_bytes([data[0], data[1]]) });
    }
    if data.len() < 4 {
        return Err("calldata is shorter than a selector".into());
    }
    let parsed = match interface().parse_call(data) {
        Ok(p) => p,
        Err(quai_sdk::abi::AbiError::NotFound) => return Ok(Call::Unknown { selector: [data[0], data[1], data[2], data[3]] }),
        Err(e) => return Err(format!("calldata does not decode: {e}")),
    };
    let a = &parsed.arguments;
    let arg = |i: usize| a.get(i).ok_or_else(|| "missing argument".to_string());
    let u = |i: usize| arg(i).and_then(|v| uint(v).ok_or_else(|| format!("argument {i} is not an integer")));
    let ad = |i: usize| arg(i).and_then(|v| address(v).ok_or_else(|| format!("argument {i} is not an address")));
    let path = |i: usize| arg(i).and_then(|v| addresses(v).filter(|p| p.len() >= 2).ok_or_else(|| "swap path is not a route".to_string()));
    let flag = |i: usize| arg(i).and_then(|v| v.as_bool().ok_or_else(|| format!("argument {i} is not a bool")));
    let name = parsed.function.name();
    let sig = parsed.function.signature();
    Ok(match (name, a.len()) {
        ("transfer", 2) => Call::Erc20Transfer { to: ad(0)?, amount: u(1)? },
        ("approve", 2) => Call::Approve { spender: ad(0)?, amount: u(1)? },
        ("setApprovalForAll", 2) => Call::ApproveAll { operator: ad(0)?, approved: flag(1)? },
        ("setApprovalForModule", 2) => Call::ModuleApproval { module: ad(0)?, approved: flag(1)? },
        ("safeTransferFrom", 3) => Call::NftTransfer { from: ad(0)?, to: ad(1)?, token_id: u(2)?, amount: None },
        ("safeTransferFrom", 5) => Call::NftTransfer { from: ad(0)?, to: ad(1)?, token_id: u(2)?, amount: Some(u(3)?) },
        (n, _) if n.starts_with("swapExactTokensFor") => Call::Swap {
            exact_output: false,
            amount_in: Some(u(0)?),
            min_out: u(1)?,
            path: path(2)?,
            native_out: n.starts_with("swapExactTokensForETH"),
            to: ad(3)?,
            deadline: u(4)?,
        },
        (n, _) if n.starts_with("swapExactETHFor") => {
            Call::Swap { exact_output: false, amount_in: None, min_out: u(0)?, path: path(1)?, native_out: false, to: ad(2)?, deadline: u(3)? }
        }
        ("swapTokensForExactTokens" | "swapTokensForExactETH", _) => Call::Swap {
            exact_output: true,
            amount_in: Some(u(1)?),
            min_out: u(0)?,
            path: path(2)?,
            native_out: name == "swapTokensForExactETH",
            to: ad(3)?,
            deadline: u(4)?,
        },
        ("swapETHForExactTokens", _) => {
            Call::Swap { exact_output: true, amount_in: None, min_out: u(0)?, path: path(1)?, native_out: false, to: ad(2)?, deadline: u(3)? }
        }
        ("addLiquidity", _) => Call::AddLiquidity {
            token_a: ad(0)?,
            token_b: Some(ad(1)?),
            amount_a: u(2)?,
            amount_b: Some(u(3)?),
            min_a: u(4)?,
            min_b: u(5)?,
            to: ad(6)?,
            deadline: u(7)?,
        },
        ("addLiquidityETH", _) => Call::AddLiquidity {
            token_a: ad(0)?,
            token_b: None,
            amount_a: u(1)?,
            amount_b: None,
            min_a: u(2)?,
            min_b: u(3)?,
            to: ad(4)?,
            deadline: u(5)?,
        },
        ("removeLiquidity", _) => Call::RemoveLiquidity {
            token_a: ad(0)?,
            token_b: Some(ad(1)?),
            liquidity: u(2)?,
            min_a: u(3)?,
            min_b: u(4)?,
            to: ad(5)?,
            deadline: u(6)?,
        },
        ("removeLiquidityETH", _) => {
            Call::RemoveLiquidity { token_a: ad(0)?, token_b: None, liquidity: u(1)?, min_a: u(2)?, min_b: u(3)?, to: ad(4)?, deadline: u(5)? }
        }
        ("deposit", 0) => Call::WrapQuai,
        ("withdraw", 1) => Call::UnwrapQuai { amount: u(0)? },
        ("claimDeposit", 0) => Call::ClaimWqi,
        ("unwrapQi", 3) => Call::UnwrapWqi { beneficiary: ad(0)?, amount: u(1)? },
        ("buy", 2) => Call::CurveBuy { min_tokens: u(0)?, deadline: Some(u(1)?) },
        ("buy", 1) => Call::CurveBuy { min_tokens: u(0)?, deadline: None },
        ("sell", 3) => Call::CurveSell { max_tokens: u(0)?, min_quote: u(1)?, deadline: Some(u(2)?) },
        ("sell", 2) => Call::CurveSell { max_tokens: u(0)?, min_quote: u(1)?, deadline: None },
        ("claimQuote", 1) => Call::CurveClaim { recipient: ad(0)? },
        ("stake", 2) => Call::GaugeStake { pid: u(0)?, amount: u(1)? },
        ("withdraw", 2) => Call::GaugeWithdraw { pid: u(0)?, amount: u(1)? },
        ("getReward", 2) => Call::GaugeReward { pid: u(0)?, exit: false },
        ("exit", 2) => Call::GaugeReward { pid: u(0)?, exit: true },
        ("notifyRewardAmount", 4) => Call::GaugeNotify { pid: u(0)?, token: ad(1)?, amount: u(2)?, duration: u(3)? },
        ("fillAsk", 5) => Call::ZoraFill { contract: ad(0)?, token_id: u(1)?, currency: ad(2)?, amount: u(3)? },
        ("createAsk", 6) => Call::ZoraCreate { contract: ad(0)?, token_id: u(1)?, price: u(2)?, currency: ad(3)?, funds_recipient: ad(4)? },
        ("setAskPrice", 4) => Call::ZoraSetPrice { contract: ad(0)?, token_id: u(1)?, price: u(2)?, currency: ad(3)? },
        ("cancelAsk", 2) => Call::ZoraCancel { contract: ad(0)?, token_id: u(1)? },
        ("post", 3) => Call::BoardPost { body_len: arg(2)?.as_str().map_or(0, |s| s.trim_start_matches("0x").len() / 2) },
        _ => return Err(format!("{sig} has no decoding rule")),
    })
}

/// A Qi-ledger address (Quai addresses have the ledger bit clear).
fn is_qi_address(address: &str) -> bool {
    address.parse::<quai_sdk::primitives::Address>().is_ok_and(|a| a.ledger() == quai_sdk::primitives::Ledger::Qi)
}

/// What the review declared, from its request.
pub struct Declared<'a> {
    pub kind: &'a OpKind,
    /// The signing account.
    pub owner: &'a str,
    /// The review's counterparty (recipient, router, contract).
    pub counterparty: &'a str,
    /// The journal amount (base units of `asset`).
    pub amount: U256,
    pub detail: &'a Detail,
    /// This network's WQUAI, for native legs of a route.
    pub wquai: Option<&'a str>,
}

/// Why a review was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch(pub String);

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the transaction's bytes do not match its review: {}", self.0)
    }
}

/// A declared financial effect, parsed.
struct Effect {
    out: bool,
    token: String,
    amount: U256,
    minimum: Option<U256>,
}

fn effects(detail: &Detail) -> Option<Vec<Effect>> {
    let list = detail.financial_effects().as_array()?;
    Some(
        list.iter()
            .filter_map(|e| {
                Some(Effect {
                    out: e["direction"].as_str()? == "out",
                    token: e["token"].as_str()?.to_lowercase(),
                    amount: uint(&e["amount"])?,
                    minimum: uint(&e["minimum"]),
                })
            })
            .collect(),
    )
}

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn text(v: &serde_json::Value) -> Option<String> {
    v.as_str().map(str::to_lowercase)
}

/// Check a decoded transaction against its review. Returns lines stating what the bytes do,
/// for the review to show; refuses on any disagreement.
pub fn check(decoded: &Decoded, declared: &Declared<'_>) -> Result<Vec<String>, Mismatch> {
    let bad = |why: String| Err(Mismatch(why));
    let d = declared.detail;
    let owner = declared.owner;
    let kind = declared.kind;
    let effects = effects(d);
    // Native value: a declared native spend, or the amount of a kind that sends it.
    let declared_native = || -> Option<U256> {
        if let Some(list) = &effects {
            let natives: Vec<&Effect> = list.iter().filter(|e| e.out && e.token == "quai").collect();
            if !natives.is_empty() {
                return Some(natives.iter().fold(U256::ZERO, |sum, e| sum.saturating_add(e.amount)));
            }
        }
        match kind {
            OpKind::SendQuai | OpKind::ConvertQuaiToQi | OpKind::WrapQuai | OpKind::CurveBuy | OpKind::HartiiBuy => Some(declared.amount),
            OpKind::ContractCall => d.native_value_atoms().or(Some(U256::ZERO)),
            _ => Some(U256::ZERO),
        }
    };
    let native = declared_native().unwrap_or(U256::ZERO);
    if decoded.value > native {
        return bad(format!("it sends {} native base units, the review declares {native}", decoded.value));
    }
    // A token spend must be a declared out effect of that token and amount, or the journal
    // amount of a kind that spends `detail.token`.
    let spends = |token: &str, amount: U256| -> bool {
        match &effects {
            Some(list) if list.iter().any(|e| e.out) => list.iter().any(|e| e.out && same(&e.token, token) && e.amount == amount),
            _ => text(d.token()).is_some_and(|t| same(&t, token)) && amount == declared.amount,
        }
    };
    let deadline_ok = |deadline: &U256| -> bool { d.expires_at().as_u64().is_none_or(|e| *deadline == U256::from(e)) };
    let min_ok = |token: &str, minimum: U256| -> bool {
        match &effects {
            Some(list) => list
                .iter()
                .filter(|e| !e.out && same(&e.token, token))
                .all(|e| e.minimum.is_none_or(|m| m == minimum)),
            None => text(d.minimum_out()).is_none_or(|m| U256::from_str_radix(&m, 10).is_ok_and(|m| m == minimum)),
        }
    };
    let target = decoded.to.as_deref().unwrap_or("");
    let to_is = |a: &str| same(target, a);
    let mut said = Vec::new();
    match &decoded.call {
        Call::NativeTransfer => {
            if !matches!(kind, OpKind::SendQuai | OpKind::FillGap | OpKind::ContractCall) {
                return bad(format!("a {kind} carries no call"));
            }
            if matches!(kind, OpKind::SendQuai | OpKind::FillGap) && !to_is(declared.counterparty) {
                return bad(format!("it pays {target}, the review names {}", declared.counterparty));
            }
            if *kind == OpKind::SendQuai && decoded.value != declared.amount {
                return bad(format!("it sends {} base units, the review {}", decoded.value, declared.amount));
            }
            said.push(format!("transfer {} QUAI to {target}", crate::amount::quai(decoded.value)));
        }
        Call::Conversion { slippage_bps } => {
            if *kind != OpKind::ConvertQuaiToQi || !to_is(declared.counterparty) || decoded.value != declared.amount {
                return bad("a conversion that is not the reviewed one".into());
            }
            if d.slippage_bps().as_u64().is_some_and(|s| s != u64::from(*slippage_bps)) {
                return bad(format!("its slippage is {slippage_bps} bps, the review {}", d.slippage_bps()));
            }
            said.push(format!("convert to Qi at {target}, refund above {slippage_bps} bps slippage"));
        }
        Call::Erc20Transfer { to, amount } => {
            if *kind != OpKind::SendToken || !same(to, declared.counterparty) || !spends(target, *amount) {
                return bad(format!("it transfers {amount} of {target} to {to}"));
            }
            said.push(format!("transfer({to}, {amount}) on {target}"));
        }
        Call::Approve { spender, amount } => {
            if !matches!(kind, OpKind::Approve | OpKind::Revoke) {
                return bad(format!("an approval inside a {kind}"));
            }
            let reviewed_spender = text(d.spender()).unwrap_or_else(|| declared.counterparty.to_lowercase());
            if !same(spender, &reviewed_spender) {
                return bad(format!("it approves {spender}, the review names {reviewed_spender}"));
            }
            if let Some(token) = text(d.token())
                && !same(&token, target)
            {
                return bad(format!("it approves on {target}, the review names the token {token}"));
            }
            let unlimited = d.unlimited().as_bool() == Some(true);
            let expected = if *kind == OpKind::Revoke { U256::ZERO } else if unlimited { U256::MAX } else { declared.amount };
            if *amount != expected {
                return bad(format!("it approves {amount}, the review {expected}"));
            }
            said.push(format!("approve({spender}, {amount}) on {target}"));
        }
        Call::ApproveAll { operator, approved } => {
            if *kind != OpKind::Approve || !approved || text(d.operator()).is_none_or(|o| !same(&o, operator)) {
                return bad(format!("an operator approval of {operator} the review does not name"));
            }
            if text(d.contract()).is_some_and(|c| !same(&c, target)) {
                return bad("the operator approval is on another collection".into());
            }
            said.push(format!("setApprovalForAll({operator}, true) on {target}"));
        }
        Call::ModuleApproval { module, approved } => {
            if *kind != OpKind::Approve || !approved || text(d.module()).is_none_or(|m| !same(&m, module)) || !to_is(declared.counterparty) {
                return bad(format!("a module approval of {module} the review does not name"));
            }
            said.push(format!("setApprovalForModule({module}, true) on {target}"));
        }
        Call::Swap { exact_output, amount_in, min_out, path, native_out, to, deadline } => {
            if *exact_output != (*kind == OpKind::SwapExactOutput) || !kind.is_router_swap() {
                return bad(format!("a router swap inside a {kind}"));
            }
            if !to_is(declared.counterparty) || text(d.router()).is_some_and(|r| !same(&r, target)) {
                return bad(format!("the swap goes through {target}, the review names {}", declared.counterparty));
            }
            let recipient = text(d.recipient()).unwrap_or_else(|| owner.to_lowercase());
            if !same(to, owner) || !same(to, &recipient) {
                return bad(format!("the swap pays {to}, not the signing account"));
            }
            let wquai = declared.wquai.map(str::to_lowercase);
            let leg = |token: Option<String>, end: &str| match token.as_deref() {
                Some("quai") => wquai.as_deref().is_some_and(|w| same(w, end)),
                Some(t) => same(t, end),
                None => true,
            };
            let first = &path[0];
            let last = &path[path.len() - 1];
            if !leg(text(d.from_token()), first) || !leg(text(d.to_token()), last) {
                return bad(format!("the swap routes {first} → {last}, not the reviewed pair"));
            }
            if (amount_in.is_none() != text(d.from_token()).is_some_and(|t| t == "quai")) && d.from_token().is_string() {
                return bad("the swap's input is not the reviewed asset".into());
            }
            if *native_out != (text(d.to_token()).as_deref() == Some("quai")) && d.to_token().is_string() {
                return bad("the swap's output is not the reviewed asset".into());
            }
            match amount_in {
                Some(amount) if !spends(first, *amount) => return bad(format!("the swap spends {amount} of {first}, not the reviewed amount")),
                None if decoded.value.is_zero() => return bad("a native swap with no value".into()),
                _ => {}
            }
            if !min_ok(if *native_out { "quai" } else { last }, *min_out) {
                return bad(format!("the swap's minimum output is {min_out}, not the reviewed minimum"));
            }
            if !deadline_ok(deadline) {
                return bad(format!("the swap's deadline is {deadline}, not the reviewed one"));
            }
            said.push(format!(
                "{} via {target}: {} → {}, {} {min_out}, to {to}, deadline {deadline}",
                if *exact_output { "exact-output swap" } else { "swap" },
                first,
                last,
                if *exact_output { "exactly" } else { "at least" }
            ));
        }
        Call::AddLiquidity { token_a, token_b, amount_a, amount_b, to, deadline, .. } => {
            if *kind != OpKind::AddLiquidity || !to_is(declared.counterparty) || !same(to, owner) || !deadline_ok(deadline) {
                return bad("a liquidity deposit that is not the reviewed one".into());
            }
            if !spends(token_a, *amount_a) || token_b.as_deref().zip(*amount_b).is_some_and(|(b, amt)| !spends(b, amt)) {
                return bad("the deposit amounts are not the reviewed ones".into());
            }
            said.push(format!("add liquidity via {target}: {amount_a} {token_a} + {} , to {to}", amount_b.map_or("native".into(), |b| format!("{b} {}", token_b.clone().unwrap_or_default()))));
        }
        Call::RemoveLiquidity { liquidity, min_a, min_b, to, deadline, .. } => {
            if *kind != OpKind::RemoveLiquidity || !to_is(declared.counterparty) || !same(to, owner) || !deadline_ok(deadline) {
                return bad("a liquidity withdrawal that is not the reviewed one".into());
            }
            if *liquidity != declared.amount {
                return bad(format!("it burns {liquidity} LP, the review {}", declared.amount));
            }
            let reviewed = |v: &serde_json::Value, got: &U256| uint(v).is_none_or(|m| m == *got);
            if !reviewed(d.amount0_min(), min_a) || !reviewed(d.amount1_min(), min_b) {
                return bad("the withdrawal's minimums are not the reviewed ones".into());
            }
            said.push(format!("remove {liquidity} LP via {target}, to {to}"));
        }
        Call::WrapQuai => {
            if *kind != OpKind::WrapQuai || !to_is(declared.counterparty) || decoded.value != declared.amount {
                return bad("a WQUAI deposit that is not the reviewed one".into());
            }
            said.push(format!("deposit {} QUAI into {target}", crate::amount::quai(decoded.value)));
        }
        Call::UnwrapQuai { amount } => {
            if *kind != OpKind::UnwrapQuai || !to_is(declared.counterparty) || *amount != declared.amount {
                return bad("a WQUAI withdrawal that is not the reviewed one".into());
            }
            said.push(format!("withdraw {amount} from {target}"));
        }
        Call::ClaimWqi => {
            if *kind != OpKind::ClaimWqi || !to_is(declared.counterparty) {
                return bad("a WQI claim that is not the reviewed one".into());
            }
            said.push(format!("claimDeposit() on {target}"));
        }
        Call::UnwrapWqi { beneficiary, amount } => {
            let contract = text(d.contract()).unwrap_or_default();
            if *kind != OpKind::UnwrapWqi || !same(target, &contract) || !same(beneficiary, declared.counterparty) || !spends(target, *amount) {
                return bad("a WQI redemption that is not the reviewed one".into());
            }
            said.push(format!("unwrapQi({beneficiary}, {amount}) on {target}"));
        }
        Call::CurveBuy { min_tokens, deadline } => {
            if !matches!(kind, OpKind::CurveBuy | OpKind::HartiiBuy) || !to_is(declared.counterparty) {
                return bad("a curve buy that is not the reviewed one".into());
            }
            let token = text(d.to_token()).or_else(|| text(d.token())).unwrap_or_default();
            if !min_ok(&token, *min_tokens) || deadline.as_ref().is_some_and(|dl| !deadline_ok(dl)) {
                return bad("the curve buy's minimum or deadline is not the reviewed one".into());
            }
            said.push(format!("buy on {target} for {} QUAI, at least {min_tokens} token base units", crate::amount::quai(decoded.value)));
        }
        Call::CurveSell { max_tokens, min_quote, deadline } => {
            if !matches!(kind, OpKind::CurveSell | OpKind::HartiiSell) || !to_is(declared.counterparty) {
                return bad("a curve sale that is not the reviewed one".into());
            }
            let token = text(d.token()).unwrap_or_default();
            if !spends(&token, *max_tokens) || !min_ok("quai", *min_quote) || deadline.as_ref().is_some_and(|dl| !deadline_ok(dl)) {
                return bad("the curve sale's amount, minimum or deadline is not the reviewed one".into());
            }
            said.push(format!("sell {max_tokens} token base units on {target}, at least {} QUAI", crate::amount::quai(*min_quote)));
        }
        Call::CurveClaim { recipient } => {
            if *kind != OpKind::CurveClaim || !to_is(declared.counterparty) || !same(recipient, owner) {
                return bad("a curve claim that is not the reviewed one".into());
            }
            said.push(format!("claimQuote({recipient}) on {target}"));
        }
        Call::GaugeStake { pid, amount } | Call::GaugeWithdraw { pid, amount } => {
            let staking = matches!(decoded.call, Call::GaugeStake { .. });
            let expected = if staking { OpKind::Stake } else { OpKind::Unstake };
            if *kind != expected || !to_is(declared.counterparty) || uint(d.pid()).is_some_and(|p| p != *pid) || *amount != declared.amount {
                return bad("a gauge call that is not the reviewed one".into());
            }
            said.push(format!("{}(pid {pid}, {amount}) on {target}", if staking { "stake" } else { "withdraw" }));
        }
        Call::GaugeReward { pid, exit } => {
            let expected = if *exit { OpKind::Exit } else { OpKind::Harvest };
            if *kind != expected || !to_is(declared.counterparty) || uint(d.pid()).is_some_and(|p| p != *pid) {
                return bad("a reward claim that is not the reviewed one".into());
            }
            said.push(format!("{}(pid {pid}) on {target}", if *exit { "exit" } else { "getReward" }));
        }
        Call::GaugeNotify { pid, token, amount, .. } => {
            if *kind != OpKind::Incentivize || !to_is(declared.counterparty) || uint(d.pid()).is_some_and(|p| p != *pid) {
                return bad("an incentive that is not the reviewed one".into());
            }
            if text(d.reward()).is_some_and(|r| !same(&r, token)) || !spends(token, *amount) {
                return bad("the incentive's token or amount is not the reviewed one".into());
            }
            said.push(format!("notifyRewardAmount(pid {pid}, {token}, {amount}) on {target}"));
        }
        Call::ZoraFill { contract, token_id, currency, amount } => {
            if *kind != OpKind::NftBuy || text(d.contract()).is_none_or(|c| !same(&c, contract)) || !token_matches(d, token_id) {
                return bad("a purchase of another item".into());
            }
            let native = same(currency, ZERO_ADDRESS);
            if (native && decoded.value != *amount) || (!native && !decoded.value.is_zero()) {
                return bad("the purchase's payment does not match its price".into());
            }
            if uint(d.price()).is_some_and(|p| p != *amount) {
                return bad(format!("it pays {amount}, the review's price is {}", d.price()));
            }
            said.push(format!("fillAsk({contract} #{token_id}) paying {amount}"));
        }
        Call::ZoraCreate { contract, token_id, price, funds_recipient, .. } | Call::ZoraSetPrice { contract, token_id, price, currency: funds_recipient } => {
            let creating = matches!(decoded.call, Call::ZoraCreate { .. });
            if !matches!(kind, OpKind::NftList | OpKind::NftReprice) || !to_is(declared.counterparty) {
                return bad("a listing that is not the reviewed one".into());
            }
            if text(d.contract()).is_none_or(|c| !same(&c, contract)) || !token_matches(d, token_id) || uint(d.price()).is_some_and(|p| p != *price) {
                return bad("the listing's item or price is not the reviewed one".into());
            }
            if creating && !same(funds_recipient, owner) {
                return bad(format!("the sale would pay {funds_recipient}, not the signing account"));
            }
            said.push(format!("list {contract} #{token_id} at {price}"));
        }
        Call::ZoraCancel { contract, token_id } => {
            if *kind != OpKind::NftUnlist || !to_is(declared.counterparty) || text(d.contract()).is_none_or(|c| !same(&c, contract)) || !token_matches(d, token_id) {
                return bad("a cancellation of another listing".into());
            }
            said.push(format!("cancelAsk({contract} #{token_id})"));
        }
        Call::NftTransfer { from, to, token_id, amount } => {
            if *kind != OpKind::NftTransfer || !same(from, owner) || !same(to, declared.counterparty) || !token_matches(d, token_id) {
                return bad("an NFT transfer that is not the reviewed one".into());
            }
            if text(d.contract()).is_some_and(|c| !same(&c, target)) {
                return bad("the transfer is on another collection".into());
            }
            if let Some(amount) = amount
                && uint(d.quantity()).is_some_and(|q| q != *amount)
            {
                return bad("the transfer's quantity is not the reviewed one".into());
            }
            said.push(format!("safeTransferFrom({from} → {to}, #{token_id}) on {target}"));
        }
        Call::BoardPost { body_len } => {
            if *kind != OpKind::BoardPost {
                return bad(format!("a board post inside a {kind}"));
            }
            said.push(format!("post a {body_len}-byte message to {target}"));
        }
        Call::Unknown { selector } => {
            if *kind != OpKind::ContractCall {
                return bad(format!("selector 0x{} is not a call a {kind} makes", hex::encode(selector)));
            }
            said.push(format!("raw call 0x{} on {target} (not decoded)", hex::encode(selector)));
        }
    }
    if *kind == OpKind::ContractCall && !matches!(decoded.call, Call::Unknown { .. }) {
        // A generic call that decodes as one of the wallet's own verbs still went through the
        // check above for that verb, so it cannot carry a different spend than the review says.
        said.push("the call matches a function the wallet knows".into());
    }
    Ok(said)
}

fn token_matches(d: &Detail, id: &U256) -> bool {
    uint(d.token_id()).is_some_and(|t| t == *id)
}

/// A reason a review needs a typed confirmation instead of a key press.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum Risk {
    /// A contract the wallet does not know; its effects were not decoded.
    UnknownContract,
    /// An approval with no upper bound.
    UnlimitedApproval,
    /// A first payment to an address this wallet has never paid.
    FirstPayment,
    /// Half or more of the account's QUAI.
    LargeShare,
}

impl Risk {
    pub fn describe(&self) -> &'static str {
        match self {
            Risk::UnknownContract => "calls a contract the wallet does not know; what it does was not decoded",
            Risk::UnlimitedApproval => "approves an unlimited amount",
            Risk::FirstPayment => "pays an address this wallet has never paid",
            Risk::LargeShare => "moves half or more of this account's QUAI",
        }
    }
}

/// The words to type for a risky review: short, and different for each kind of risk, so a
/// confirmation typed for one review is not the same keystrokes as the next.
pub fn confirm_phrase(risks: &[Risk], to: &str) -> Option<String> {
    let first = risks.first()?;
    let tail: String = to.trim_start_matches("0x").chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect::<String>().to_lowercase();
    Some(match first {
        Risk::UnknownContract => format!("call {tail}"),
        Risk::UnlimitedApproval => format!("unlimited {tail}"),
        Risk::FirstPayment => format!("pay {tail}"),
        Risk::LargeShare => format!("send {tail}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;
    use serde_json::json;

    const OWNER: &str = "0x00aa000000000000000000000000000000000001";
    const ROUTER: &str = "0x00bb000000000000000000000000000000000002";
    const TOKEN: &str = "0x00cc000000000000000000000000000000000003";
    const OUT: &str = "0x00dd000000000000000000000000000000000004";
    const WQUAI: &str = "0x00ee000000000000000000000000000000000005";
    const EVIL: &str = "0x00ff000000000000000000000000000000000006";

    fn encode(signature: &str, args: &[serde_json::Value]) -> Vec<u8> {
        let abi = quai_sdk::abi::AbiInterface::from_human_readable(&[signature]).unwrap();
        let name = signature.trim_start_matches("function ").split('(').next().unwrap();
        abi.function(name).unwrap().encode_call(args).unwrap()
    }

    const SWAP: &str = "function swapExactTokensForTokens(uint256 amountIn, uint256 amountOutMin, address[] path, address to, uint256 deadline)";

    fn swap_detail() -> Detail {
        Detail::from(json!({
            "from_token": TOKEN, "to_token": OUT, "router": ROUTER, "recipient": OWNER, "expires_at": 1_000,
            "minimum_out": "90",
            "financial_effects": [
                {"direction": "out", "asset": "T", "token": TOKEN, "decimals": 18, "amount": "100", "estimated": false},
                {"direction": "in", "asset": "O", "token": OUT, "decimals": 18, "amount": "95", "minimum": "90", "estimated": true}
            ]
        }))
    }

    fn swap_bytes(amount: &str, min: &str, path: [&str; 2], to: &str, deadline: u64) -> Vec<u8> {
        encode(SWAP, &[json!(amount), json!(min), json!(path), json!(to), json!(deadline.to_string())])
    }

    fn check_swap(bytes: &[u8], value: U256, target: &str) -> Result<Vec<String>, Mismatch> {
        let detail = swap_detail();
        let decoded = decode(Some(target), value, bytes).map_err(Mismatch)?;
        check(&decoded, &Declared { kind: &OpKind::Swap, owner: OWNER, counterparty: ROUTER, amount: U256::from(100), detail: &detail, wquai: Some(WQUAI) })
    }

    #[test]
    fn the_reviewed_swap_passes_and_says_what_it_does() {
        let said = check_swap(&swap_bytes("100", "90", [TOKEN, OUT], OWNER, 1_000), U256::ZERO, ROUTER).unwrap();
        assert!(said[0].contains("at least 90") && said[0].contains(OWNER), "{said:?}");
    }

    #[test]
    fn every_tampered_swap_field_is_refused() {
        for (bytes, value, target, why) in [
            (swap_bytes("101", "90", [TOKEN, OUT], OWNER, 1_000), U256::ZERO, ROUTER, "spends more"),
            (swap_bytes("100", "1", [TOKEN, OUT], OWNER, 1_000), U256::ZERO, ROUTER, "lower minimum"),
            (swap_bytes("100", "90", [TOKEN, EVIL], OWNER, 1_000), U256::ZERO, ROUTER, "another output token"),
            (swap_bytes("100", "90", [EVIL, OUT], OWNER, 1_000), U256::ZERO, ROUTER, "another input token"),
            (swap_bytes("100", "90", [TOKEN, OUT], EVIL, 1_000), U256::ZERO, ROUTER, "pays a stranger"),
            (swap_bytes("100", "90", [TOKEN, OUT], OWNER, 99_999), U256::ZERO, ROUTER, "later deadline"),
            (swap_bytes("100", "90", [TOKEN, OUT], OWNER, 1_000), U256::from(1), ROUTER, "carries native value"),
            (swap_bytes("100", "90", [TOKEN, OUT], OWNER, 1_000), U256::ZERO, EVIL, "another router"),
        ] {
            assert!(check_swap(&bytes, value, target).is_err(), "{why} was accepted");
        }
    }

    #[test]
    fn approvals_must_name_the_reviewed_spender_and_amount() {
        let approve = "function approve(address spender, uint256 amount)";
        let detail = Detail::from(json!({"spender": ROUTER, "token": TOKEN}));
        let declared = |kind: &'static OpKind, detail: &'static Detail| Declared { kind, owner: OWNER, counterparty: ROUTER, amount: U256::from(100), detail, wquai: None };
        let d: &'static Detail = Box::leak(Box::new(detail));
        let run = |spender: &str, amount: &str, on: &str| {
            let decoded = decode(Some(on), U256::ZERO, &encode(approve, &[json!(spender), json!(amount)])).unwrap();
            check(&decoded, &declared(&OpKind::Approve, d))
        };
        assert!(run(ROUTER, "100", TOKEN).is_ok());
        assert!(run(EVIL, "100", TOKEN).is_err(), "another spender");
        assert!(run(ROUTER, "101", TOKEN).is_err(), "more than reviewed");
        assert!(run(ROUTER, &U256::MAX.to_string(), TOKEN).is_err(), "unlimited when the review said exact");
        assert!(run(ROUTER, "100", EVIL).is_err(), "on another token");
    }

    #[test]
    fn native_sends_and_unknown_calls() {
        let detail = Detail::new();
        let send = |value: u64, to: &str, data: &[u8], kind: &'static OpKind| {
            let decoded = decode(Some(to), U256::from(value), data).unwrap();
            check(&decoded, &Declared { kind, owner: OWNER, counterparty: OUT, amount: U256::from(5), detail: &detail, wquai: None })
        };
        assert!(send(5, OUT, &[], &OpKind::SendQuai).is_ok());
        assert!(send(6, OUT, &[], &OpKind::SendQuai).is_err());
        assert!(send(5, EVIL, &[], &OpKind::SendQuai).is_err());
        // Calldata the wallet does not know: only a generic contract call may carry it.
        let unknown = [0xde, 0xad, 0xbe, 0xef, 0, 1];
        assert!(send(0, OUT, &unknown, &OpKind::ContractCall).is_ok());
        assert!(send(0, OUT, &unknown, &OpKind::SendQuai).is_err());
        // A known verb hidden in a generic call is still held to what the review declares.
        let transfer = encode("function transfer(address to, uint256 amount)", &[json!(EVIL), json!("1")]);
        assert!(send(0, OUT, &transfer, &OpKind::ContractCall).is_err());
    }

    #[test]
    fn truncated_calldata_under_a_known_selector_is_refused() {
        let bytes = swap_bytes("100", "90", [TOKEN, OUT], OWNER, 1_000);
        assert!(decode(Some(ROUTER), U256::ZERO, &bytes[..bytes.len() - 1]).is_err());
        assert!(decode(Some(ROUTER), U256::ZERO, &bytes[..3]).is_err());
    }

    /// Fuzz: mutated calldata of a valid swap is either refused, or decodes to exactly the
    /// economics that were reviewed. Nothing the review did not state gets through.
    #[test]
    fn fuzz_mutated_swaps_never_pass_with_different_economics() {
        let good = swap_bytes("100", "90", [TOKEN, OUT], OWNER, 1_000);
        let reference = decode(Some(ROUTER), U256::ZERO, &good).unwrap();
        let iterations: u64 = std::env::var("QW_FUZZ_ITERATIONS").ok().and_then(|v| v.parse().ok()).unwrap_or(20_000);
        for seed in 0..iterations {
            let mut rng = Rng::new(seed);
            let mut bytes = good.clone();
            for _ in 0..1 + rng.below(4) {
                let i = rng.below(bytes.len());
                match rng.below(3) {
                    0 => bytes[i] = rng.next() as u8,
                    1 => bytes[i] ^= 1 << rng.below(8),
                    _ => bytes.truncate(i.max(1)),
                }
            }
            if check_swap(&bytes, U256::ZERO, ROUTER).is_ok() {
                let decoded = decode(Some(ROUTER), U256::ZERO, &bytes).unwrap();
                assert_eq!(decoded, reference, "seed {seed}: accepted different economics");
            }
        }
    }

    /// Fuzz: the decoder never panics on any bytes, to any address.
    #[test]
    fn fuzz_decoder_never_panics() {
        for seed in 0..20_000u64 {
            let mut rng = Rng::new(seed);
            let len = rng.below(200);
            let mut bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            if rng.below(2) == 0 && bytes.len() >= 4 {
                // Real selectors with garbage behind them.
                let known = interface().functions().nth(rng.below(DECLARATIONS.len())).map(|f| f.selector());
                if let Some(sel) = known {
                    bytes[..4].copy_from_slice(&sel);
                }
            }
            let _ = decode(Some(ROUTER), U256::from(rng.next()), &bytes);
        }
    }

    #[test]
    fn every_declaration_has_a_decoding_rule() {
        for f in interface().functions() {
            let args: Vec<serde_json::Value> = f
                .inputs()
                .iter()
                .map(|p| match p.canonical_name().as_str() {
                    "address" => json!(OWNER),
                    "address[]" => json!([TOKEN, OUT]),
                    "bool" => json!(true),
                    "bytes" => json!("0x0102"),
                    "bytes32" => json!(format!("0x{}", "11".repeat(32))),
                    _ => json!("7"),
                })
                .collect();
            let bytes = f.encode_call(&args).unwrap();
            let decoded = decode(Some(ROUTER), U256::ZERO, &bytes).unwrap_or_else(|e| panic!("{}: {e}", f.signature()));
            assert!(!matches!(decoded.call, Call::Unknown { .. }), "{}", f.signature());
        }
    }

    /// Each verb the builders make, shaped as its builder shapes the detail: the reviewed bytes
    /// pass, and a tampered copy (one field changed) is refused.
    #[test]
    fn every_builder_shape_passes_and_its_tampered_twin_is_refused() {
        struct Case {
            name: &'static str,
            kind: OpKind,
            target: &'static str,
            counterparty: &'static str,
            value: u64,
            amount: u64,
            detail: serde_json::Value,
            good: Vec<u8>,
            bad: Vec<u8>,
            bad_value: Option<u64>,
        }
        let e = |dir: &str, token: &str, amount: &str| json!({"direction": dir, "asset": "X", "token": token, "decimals": 18, "amount": amount, "estimated": false});
        let em = |token: &str, amount: &str, min: &str| json!({"direction": "in", "asset": "X", "token": token, "decimals": 18, "amount": amount, "minimum": min, "estimated": true});
        let cases = vec![
            Case {
                name: "quainance curve buy",
                kind: OpKind::CurveBuy,
                target: ROUTER,
                counterparty: ROUTER,
                value: 50,
                amount: 50,
                detail: json!({"token": TOKEN, "to_token": TOKEN, "expires_at": 900, "financial_effects": [e("out", "quai", "50"), em(TOKEN, "70", "60")]}),
                good: encode("function buy(uint256 minimumTokenAmount, uint256 deadline)", &[json!("60"), json!("900")]),
                bad: encode("function buy(uint256 minimumTokenAmount, uint256 deadline)", &[json!("1"), json!("900")]),
                bad_value: None,
            },
            Case {
                name: "hartii curve buy",
                kind: OpKind::HartiiBuy,
                target: ROUTER,
                counterparty: ROUTER,
                value: 50,
                amount: 50,
                detail: json!({"token": TOKEN, "to_token": TOKEN, "minimum_out": "60", "financial_effects": [e("out", "quai", "50"), em(TOKEN, "70", "60")]}),
                good: encode("function buy(uint256 minTokensOut)", &[json!("60")]),
                bad: encode("function buy(uint256 minTokensOut)", &[json!("59")]),
                bad_value: Some(51),
            },
            Case {
                name: "hartii curve sell",
                kind: OpKind::HartiiSell,
                target: ROUTER,
                counterparty: ROUTER,
                value: 0,
                amount: 40,
                detail: json!({"token": TOKEN, "to_token": "quai", "financial_effects": [e("out", TOKEN, "40"), em("quai", "30", "25")]}),
                good: encode("function sell(uint256 tokensIn, uint256 minQuaiOut)", &[json!("40"), json!("25")]),
                bad: encode("function sell(uint256 tokensIn, uint256 minQuaiOut)", &[json!("41"), json!("25")]),
                bad_value: Some(1),
            },
            Case {
                name: "gauge stake",
                kind: OpKind::Stake,
                target: ROUTER,
                counterparty: ROUTER,
                value: 0,
                amount: 10,
                detail: json!({"pid": 3, "pair": TOKEN, "financial_effects": [e("out", TOKEN, "10")]}),
                good: encode("function stake(uint256 pid, uint256 amount)", &[json!("3"), json!("10")]),
                bad: encode("function stake(uint256 pid, uint256 amount)", &[json!("4"), json!("10")]),
                bad_value: None,
            },
            Case {
                name: "add liquidity",
                kind: OpKind::AddLiquidity,
                target: ROUTER,
                counterparty: ROUTER,
                value: 0,
                amount: 5,
                detail: json!({"expires_at": 900, "financial_effects": [e("out", TOKEN, "5"), e("out", OUT, "6")]}),
                good: encode(
                    "function addLiquidity(address tokenA, address tokenB, uint256 amountADesired, uint256 amountBDesired, uint256 amountAMin, uint256 amountBMin, address to, uint256 deadline)",
                    &[json!(TOKEN), json!(OUT), json!("5"), json!("6"), json!("4"), json!("5"), json!(OWNER), json!("900")],
                ),
                bad: encode(
                    "function addLiquidity(address tokenA, address tokenB, uint256 amountADesired, uint256 amountBDesired, uint256 amountAMin, uint256 amountBMin, address to, uint256 deadline)",
                    &[json!(TOKEN), json!(OUT), json!("5"), json!("6"), json!("4"), json!("5"), json!(EVIL), json!("900")],
                ),
                bad_value: None,
            },
            Case {
                name: "remove liquidity",
                kind: OpKind::RemoveLiquidity,
                target: ROUTER,
                counterparty: ROUTER,
                value: 0,
                amount: 9,
                detail: json!({"expires_at": 900, "amount0_min": "3", "amount1_min": "4"}),
                good: encode(
                    "function removeLiquidity(address tokenA, address tokenB, uint256 liquidity, uint256 amountAMin, uint256 amountBMin, address to, uint256 deadline)",
                    &[json!(TOKEN), json!(OUT), json!("9"), json!("3"), json!("4"), json!(OWNER), json!("900")],
                ),
                bad: encode(
                    "function removeLiquidity(address tokenA, address tokenB, uint256 liquidity, uint256 amountAMin, uint256 amountBMin, address to, uint256 deadline)",
                    &[json!(TOKEN), json!(OUT), json!("9"), json!("0"), json!("4"), json!(OWNER), json!("900")],
                ),
                bad_value: None,
            },
            Case {
                name: "wrap QUAI",
                kind: OpKind::WrapQuai,
                target: WQUAI,
                counterparty: WQUAI,
                value: 7,
                amount: 7,
                detail: json!({"contract": WQUAI, "financial_effects": [e("out", "quai", "7"), e("in", WQUAI, "7")]}),
                good: encode("function deposit()", &[]),
                bad: encode("function withdraw(uint256 amount)", &[json!("7")]),
                bad_value: Some(8),
            },
            Case {
                name: "unwrap WQI",
                kind: OpKind::UnwrapWqi,
                target: TOKEN,
                counterparty: OWNER,
                value: 0,
                amount: 8,
                detail: json!({"contract": TOKEN, "beneficiary": OWNER, "financial_effects": [e("out", TOKEN, "8")]}),
                good: encode("function unwrapQi(address beneficiary, uint256 amount, uint64 etxGas)", &[json!(OWNER), json!("8"), json!("21000")]),
                bad: encode("function unwrapQi(address beneficiary, uint256 amount, uint64 etxGas)", &[json!(EVIL), json!("8"), json!("21000")]),
                bad_value: None,
            },
            Case {
                name: "NFT transfer",
                kind: OpKind::NftTransfer,
                target: TOKEN,
                counterparty: OUT,
                value: 0,
                amount: 1,
                detail: json!({"contract": TOKEN, "token_id": "12"}),
                good: encode("function safeTransferFrom(address from, address to, uint256 tokenId)", &[json!(OWNER), json!(OUT), json!("12")]),
                bad: encode("function safeTransferFrom(address from, address to, uint256 tokenId)", &[json!(OWNER), json!(OUT), json!("13")]),
                bad_value: None,
            },
            Case {
                name: "Zora purchase",
                kind: OpKind::NftBuy,
                target: ROUTER,
                counterparty: EVIL,
                value: 100,
                amount: 100,
                detail: json!({"contract": TOKEN, "token_id": "12", "price": "100", "financial_effects": [e("out", "quai", "100")]}),
                good: encode(
                    "function fillAsk(address tokenContract, uint256 tokenId, address fillCurrency, uint256 fillAmount, address finder)",
                    &[json!(TOKEN), json!("12"), json!(ZERO_ADDRESS), json!("100"), json!(OWNER)],
                ),
                bad: encode(
                    "function fillAsk(address tokenContract, uint256 tokenId, address fillCurrency, uint256 fillAmount, address finder)",
                    &[json!(TOKEN), json!("13"), json!(ZERO_ADDRESS), json!("100"), json!(OWNER)],
                ),
                bad_value: Some(99),
            },
            Case {
                name: "Zora listing",
                kind: OpKind::NftList,
                target: ROUTER,
                counterparty: ROUTER,
                value: 0,
                amount: 1,
                detail: json!({"contract": TOKEN, "token_id": "12", "price": "500"}),
                good: encode(
                    "function createAsk(address tokenContract, uint256 tokenId, uint256 askPrice, address askCurrency, address sellerFundsRecipient, uint16 findersFeeBps)",
                    &[json!(TOKEN), json!("12"), json!("500"), json!(ZERO_ADDRESS), json!(OWNER), json!("0")],
                ),
                bad: encode(
                    "function createAsk(address tokenContract, uint256 tokenId, uint256 askPrice, address askCurrency, address sellerFundsRecipient, uint16 findersFeeBps)",
                    &[json!(TOKEN), json!("12"), json!("500"), json!(ZERO_ADDRESS), json!(EVIL), json!("0")],
                ),
                bad_value: None,
            },
        ];
        for c in cases {
            let detail = Detail::from(c.detail.clone());
            let declared = Declared { kind: &c.kind, owner: OWNER, counterparty: c.counterparty, amount: U256::from(c.amount), detail: &detail, wquai: Some(WQUAI) };
            let run = |bytes: &[u8], value: u64| {
                let decoded = decode(Some(c.target), U256::from(value), bytes).map_err(Mismatch)?;
                check(&decoded, &declared)
            };
            run(&c.good, c.value).unwrap_or_else(|m| panic!("{}: the reviewed bytes were refused: {m}", c.name));
            assert!(run(&c.bad, c.value).is_err(), "{}: tampered bytes accepted", c.name);
            if let Some(v) = c.bad_value {
                assert!(run(&c.good, v).is_err(), "{}: another native value accepted", c.name);
            }
        }
    }

    #[test]
    fn risky_reviews_get_a_phrase_of_their_own() {
        assert_eq!(confirm_phrase(&[], OUT), None);
        assert_eq!(confirm_phrase(&[Risk::UnknownContract], "0x00DD000000000000000000000000000000000004").as_deref(), Some("call 0004"));
        assert_ne!(confirm_phrase(&[Risk::UnlimitedApproval], OUT), confirm_phrase(&[Risk::FirstPayment], OUT));
    }
}
