//! Explorer API backends: explorer.qu.ai (mainnet), Blockscout v2 (Orchard) and chain-only.
//!
//! Everything returned here is untrusted, indexer-sourced display data carrying its source and
//! observation time. Review and signing never rely on it: balances are re-read on-chain, and NFT
//! ownership and asks are re-checked before any transfer or purchase. Unsupported calls return
//! [`CoreError::Unsupported`]-style `NotFound` errors that the UI shows as "—".

use crate::amount::parse_indexer_integer;
use crate::error::{CoreError, Result};
use crate::http;
use crate::network::{ExplorerKind, NetworkProfile};
use crate::registry::now;
use quai_sdk::U256;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Which backend serves a network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backend {
    /// explorer.qu.ai.
    Quai,
    /// Blockscout v2.
    Blockscout,
    /// No explorer: only on-chain reads.
    ChainOnly,
}

/// An explorer bound to a network.
#[derive(Clone, Debug)]
pub struct Explorer {
    /// Backend flavor.
    pub backend: Backend,
    /// Base URL without trailing slash (empty for chain-only).
    pub base: String,
}

/// Token standard.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "UPPERCASE")]
pub enum TokenKind {
    /// Fungible token.
    Erc20,
    /// Non-fungible token.
    Erc721,
    /// Multi-token.
    Erc1155,
}

impl TokenKind {
    fn parse(text: &str) -> Option<TokenKind> {
        match text.to_ascii_uppercase().replace('-', "").as_str() {
            "ERC20" => Some(TokenKind::Erc20),
            "ERC721" => Some(TokenKind::Erc721),
            "ERC1155" => Some(TokenKind::Erc1155),
            _ => None,
        }
    }

    /// Display name.
    pub fn label(self) -> &'static str {
        match self {
            TokenKind::Erc20 => "ERC-20",
            TokenKind::Erc721 => "ERC-721",
            TokenKind::Erc1155 => "ERC-1155",
        }
    }
}

/// QUAI and Qi USD prices.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PriceBoard {
    /// USD per QUAI.
    pub quai_usd: Option<f64>,
    /// QUAI price source (e.g. `mexc`).
    pub quai_source: String,
    /// USD per Qi (protocol-derived).
    pub qi_usd: Option<f64>,
    /// Qi price source (e.g. `derived:protocol-rate`).
    pub qi_source: String,
    /// When the explorer took the prices (unix seconds).
    pub taken_at: u64,
    /// When the wallet fetched them (unix seconds).
    pub observed_at: u64,
}

/// Market data for one token.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct TokenMarket {
    /// Contract address (lowercase).
    pub address: String,
    /// Symbol (untrusted).
    pub symbol: String,
    /// Name (untrusted).
    pub name: String,
    /// USD price.
    pub price_usd: Option<f64>,
    /// Price provenance.
    pub price_source: String,
    /// Holder count.
    pub holders: Option<u64>,
    /// 24h market-cap change in percent (a price proxy for fixed-supply tokens).
    pub cap_growth_24h: Option<f64>,
    /// Icon URL (absolute), when the explorer has one.
    pub icon_url: Option<String>,
    /// When the price was observed (unix seconds).
    pub price_at: u64,
}

/// A token balance reported by the indexer.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Holding {
    /// Contract address (lowercase).
    pub token: String,
    /// Symbol (untrusted).
    pub symbol: String,
    /// Name (untrusted).
    pub name: String,
    /// Decimals (ERC-20).
    pub decimals: Option<u8>,
    /// Standard.
    pub kind: TokenKind,
    /// Balance in base units (NFTs: item count). May be rounded by the indexer.
    #[serde(with = "u256_string")]
    pub balance: U256,
    /// Icon URL (absolute).
    pub icon_url: Option<String>,
}

/// A token or NFT transfer involving an address.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TokenTransfer {
    /// Transaction hash.
    pub tx_hash: String,
    /// Log index.
    pub log_index: u64,
    /// Block height.
    pub block: u64,
    /// Unix seconds.
    pub timestamp: u64,
    /// Sender.
    pub from: String,
    /// Recipient.
    pub to: String,
    /// Amount (NFTs: quantity).
    #[serde(with = "u256_string")]
    pub value: U256,
    /// NFT id.
    pub token_id: Option<String>,
    /// Contract (lowercase).
    pub token: String,
    /// Symbol.
    pub symbol: String,
    /// Name.
    pub name: String,
    /// Decimals.
    pub decimals: Option<u8>,
    /// Standard.
    pub kind: TokenKind,
}

/// One change in an address's native balance.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BalanceChange {
    /// Unix seconds.
    pub timestamp: u64,
    /// Block height.
    pub block: u64,
    /// Signed change in base units, as a decimal string (`-12`, `5000`).
    pub delta: String,
    /// `QUAI` or `QI`.
    pub coin: String,
}

/// Liquidity of one DEX pool (explorer.qu.ai `/api/stats/tvl`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PoolStats {
    /// Pair address (lowercase).
    pub address: String,
    /// Pair name (`USDT/WQUAI`).
    pub name: String,
    /// Total value locked (USD).
    pub tvl_usd: Option<f64>,
    /// 24h volume (USD).
    pub volume_24h_usd: Option<f64>,
}

/// Pool liquidity for a DEX, with the indexer's observation time.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PoolBoard {
    /// Pools.
    pub pools: Vec<PoolStats>,
    /// When the indexer observed them (unix seconds).
    pub observed_at: u64,
    /// The indexer itself reports stale data.
    pub stale: bool,
}

/// The explorer's step-by-step preview of a conversion (`/api/convert/quote`). Explanation
/// only: the node's `quai_calculateConversionAmount` estimate stays authoritative.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ConversionSteps {
    /// `quai-to-qi` or `qi-to-quai`.
    pub direction: String,
    /// Input valued in QUAI (Its).
    pub value_in_quai: String,
    /// Block conversion flow amount (Its).
    pub flow_amount: String,
    /// Value after the cubic flow discount (Its).
    pub after_flow_discount: String,
    /// The kQuai discount was applied.
    pub kquai_applied: bool,
    /// kQuai discount amount.
    pub kquai_discount: String,
    /// The output was floored at 10% of the input value.
    pub floored_at_10pct: bool,
    /// Value out in QUAI terms (Its).
    pub value_out_quai_terms: String,
    /// Output in destination base units (Qits for QUAI → Qi, Its for Qi → QUAI).
    pub value_out: String,
}

impl ConversionSteps {
    /// Human lines for a quote card.
    pub fn lines(&self) -> Vec<String> {
        let q = |v: &str| format!("{} QUAI", crate::amount::format_amount_short(parse_indexer_integer(v).unwrap_or_default(), 18, 4));
        let mut out = vec![format!("input worth {}", q(&self.value_in_quai))];
        if self.floored_at_10pct {
            out.push(format!("flow discount hits the floor: 10% of input, {}", q(&self.value_out_quai_terms)));
        } else {
            out.push(format!("after the block flow discount {}", q(&self.after_flow_discount)));
        }
        if self.kquai_applied && parse_indexer_integer(&self.kquai_discount).is_some_and(|d| !d.is_zero()) {
            out.push(format!("kQuai discount −{}", q(&self.kquai_discount)));
        }
        let out_value = parse_indexer_integer(&self.value_out).unwrap_or_default();
        out.push(if self.direction == "quai-to-qi" {
            format!("out {} Qi", crate::amount::qi(out_value))
        } else {
            format!("out {} QUAI", crate::amount::format_amount_short(out_value, 18, 4))
        });
        out
    }
}

/// Lockups cross-check.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct LockupSummary {
    /// Number of locked entries.
    pub total: u64,
    /// Indexed head.
    pub head: u64,
}

/// An NFT with metadata.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct NftItem {
    /// Collection contract (lowercase).
    pub contract: String,
    /// Token id (decimal).
    pub token_id: String,
    /// Standard.
    pub kind: Option<TokenKind>,
    /// Item name (untrusted).
    pub name: String,
    /// Collection name (untrusted).
    pub collection: String,
    /// Description (untrusted).
    pub description: String,
    /// Image to fetch: explorer media proxy URL when cached there, else the metadata URI.
    pub image: Option<String>,
    /// Traits (type, value).
    pub traits: Vec<(String, String)>,
    /// Owner reported by the indexer (re-verified on-chain before any action).
    pub owner: Option<String>,
    /// Quantity held (ERC-1155), as a decimal string.
    pub quantity: String,
}

/// A collection directory entry.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Collection {
    /// Contract (lowercase).
    pub address: String,
    /// Name (untrusted).
    pub name: String,
    /// Symbol (untrusted).
    pub symbol: String,
    /// Standard.
    pub kind: Option<TokenKind>,
    /// Holders.
    pub holders: Option<u64>,
    /// Preview image (media proxy).
    pub preview: Option<String>,
    /// Floor price in QUAI, when the explorer tracks one.
    pub floor_quai: Option<f64>,
    /// Floor in USD.
    pub floor_usd: Option<f64>,
}

/// Token metadata from the explorer.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct TokenInfo {
    /// Contract (lowercase).
    pub address: String,
    /// Name.
    pub name: String,
    /// Symbol.
    pub symbol: String,
    /// Decimals.
    pub decimals: Option<u8>,
    /// Holders.
    pub holders: Option<u64>,
    /// Standard.
    pub kind: Option<TokenKind>,
    /// Icon URL.
    pub icon_url: Option<String>,
}

pub(crate) mod u256_string {
    use quai_sdk::U256;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &U256, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<U256, D::Error> {
        let text = String::deserialize(d)?;
        U256::from_str_radix(&text, 10).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------- small parsers

/// Untrusted text made safe to show: control characters (C0 and C1, so no terminal escapes),
/// bidirectional overrides and zero-width characters removed, at most `max_chars` kept. The one
/// sanitizer every display path uses.
///
/// Also removed: what makes a terminal and a width table disagree about how wide the text is,
/// which would shift everything after it on the row (a column, a border). Variation selectors
/// (`❤️` is one cell by the table and two on screen), tag characters, skin-tone modifiers,
/// regional indicators (flags) and the keycap mark. Plain emoji stay: both agree they are two.
pub fn clean(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(*c,
                    '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}'
                    // Width: variation selectors, the keycap mark, flags, skin tones, tags.
                    | '\u{FE00}'..='\u{FE0F}' | '\u{20E3}' | '\u{1F1E6}'..='\u{1F1FF}' | '\u{1F3FB}'..='\u{1F3FF}' | '\u{E0000}'..='\u{E007F}' | '\u{E0100}'..='\u{E01EF}')
        })
        .take(max_chars)
        .collect()
}

/// [`clean`] for explorer and indexer text (names, descriptions, messages).
pub fn clean_text(text: &str) -> String {
    clean(text, 4096)
}

fn s(v: &Value) -> String {
    match v {
        Value::String(t) => clean_text(t),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn opt_s(v: &Value) -> Option<String> {
    let t = s(v);
    (!t.is_empty()).then_some(t)
}

fn num_u64(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64)),
        Value::String(t) => t.trim().parse::<u64>().ok().or_else(|| t.trim().parse::<f64>().ok().filter(|f| *f >= 0.0).map(|f| f as u64)),
        _ => None,
    }
}

fn num_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(t) => t.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|f| f.is_finite())
}

fn integer(v: &Value) -> Option<U256> {
    match v {
        Value::String(t) => parse_indexer_integer(t),
        Value::Number(n) => parse_indexer_integer(&n.to_string()),
        _ => None,
    }
}

fn lower_address(v: &Value) -> String {
    let text = match v {
        Value::Object(o) => o.get("hash").map(s).unwrap_or_default(),
        other => s(other),
    };
    text.to_lowercase()
}

/// Unix seconds from an RFC 3339 UTC timestamp (`2026-09-15T11:49:15.299Z`).
pub fn parse_timestamp(text: &str) -> Option<u64> {
    let t = text.trim();
    let (date, time) = t.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let time = time.trim_end_matches('Z');
    let time = time.split(['+']).next()?;
    let mut parts = time.split(':');
    let hh = parts.next()?.parse::<i64>().ok()?;
    let mm = parts.next()?.parse::<i64>().ok()?;
    let ss = parts.next().and_then(|p| p.split('.').next()).and_then(|p| p.parse::<i64>().ok()).unwrap_or(0);
    // Howard Hinnant's days-from-civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hh * 3600 + mm * 60 + ss).ok()
}

impl Explorer {
    /// The explorer for a network profile.
    pub fn for_network(profile: &NetworkProfile) -> Explorer {
        match &profile.explorer_api {
            Some(c) if c.kind == ExplorerKind::QuaiExplorer => {
                Explorer { backend: Backend::Quai, base: c.base_url.trim_end_matches('/').to_string() }
            }
            Some(c) => Explorer { backend: Backend::Blockscout, base: c.base_url.trim_end_matches('/').to_string() },
            None => Explorer { backend: Backend::ChainOnly, base: String::new() },
        }
    }

    /// Short source label (host) for provenance lines.
    pub fn source(&self) -> String {
        if self.backend == Backend::ChainOnly {
            return "chain".into();
        }
        http::host_of(&self.base).unwrap_or_else(|_| self.base.clone())
    }

    fn unsupported(&self, what: &str) -> CoreError {
        CoreError::NotFound(format!("{what} is not available from {}", self.source()))
    }

    /// Absolute URL for an explorer-relative path (`/api/nft-media/…`, `/token-icons/…`).
    /// Absolute http(s) URLs are returned unchanged; anything else (ipfs:, data:) is returned
    /// as-is for the media layer to resolve or refuse.
    pub fn absolute(&self, path: &str) -> String {
        let p = path.trim();
        if p.starts_with('/') && !self.base.is_empty() { format!("{}{p}", self.base) } else { p.to_string() }
    }

    async fn get(&self, path: &str) -> Result<Value> {
        http::get_json(&format!("{}{path}", self.base)).await
    }

    // ------------------------------------------------------------ prices and markets

    /// QUAI and Qi USD prices (mainnet explorer only).
    pub async fn prices(&self) -> Result<PriceBoard> {
        match self.backend {
            Backend::Quai => Ok(parse_prices(&self.get("/api/price/current").await?)),
            _ => Err(self.unsupported("prices")),
        }
    }

    /// Bulk token market data (top tokens by the explorer's ranking).
    pub async fn token_markets(&self) -> Result<Vec<TokenMarket>> {
        match self.backend {
            Backend::Quai => Ok(parse_stats_assets(&self.get("/api/stats/assets?kind=token&limit=100").await?, self)),
            _ => Err(self.unsupported("token prices")),
        }
    }

    /// Market quote for one token.
    pub async fn token_quote(&self, token: &str) -> Result<(f64, String, u64)> {
        match self.backend {
            Backend::Quai => {
                let v = self.get(&format!("/api/token/{}/quote", token.to_lowercase())).await?;
                let price = num_f64(&v["price"]["usd"]).ok_or_else(|| CoreError::NotFound("no price for this token".into()))?;
                Ok((price, s(&v["price"]["source"]), v["price"]["observedAt"].as_str().and_then(parse_timestamp).unwrap_or(0)))
            }
            _ => Err(self.unsupported("token quotes")),
        }
    }

    /// Quainance pool liquidity (mainnet explorer only).
    pub async fn pool_stats(&self) -> Result<PoolBoard> {
        match self.backend {
            Backend::Quai => Ok(parse_tvl(&self.get("/api/stats/tvl").await?)),
            _ => Err(self.unsupported("pool liquidity")),
        }
    }

    /// Conversion step preview (mainnet explorer only). `direction` is `quai_to_qi` or
    /// `qi_to_quai`; `amount` is in source base units.
    pub async fn convert_steps(&self, direction: &str, amount: &str) -> Result<ConversionSteps> {
        match self.backend {
            Backend::Quai => {
                let dir = direction.replace('_', "-");
                if !matches!(dir.as_str(), "quai-to-qi" | "qi-to-quai") || !amount.chars().all(|c| c.is_ascii_digit()) {
                    return Err(CoreError::Invalid("conversion direction or amount".into()));
                }
                Ok(parse_convert_steps(&self.get(&format!("/api/convert/quote?direction={dir}&amount={amount}")).await?))
            }
            _ => Err(self.unsupported("conversion previews")),
        }
    }

    /// Token metadata and holder count.
    pub async fn token_info(&self, token: &str) -> Result<TokenInfo> {
        match self.backend {
            Backend::Quai => Ok(parse_token_info(&self.get(&format!("/api/token/{}", token.to_lowercase())).await?["token"], self)),
            Backend::Blockscout => Ok(parse_blockscout_token(&self.get(&format!("/api/v2/tokens/{token}")).await?, self)),
            Backend::ChainOnly => Err(self.unsupported("token metadata")),
        }
    }

    /// Whether the explorer has verified source for a contract.
    pub async fn contract_verified(&self, address: &str) -> Result<bool> {
        match self.backend {
            Backend::Quai => {
                Ok(self.get(&format!("/api/contract/{}", address.to_lowercase())).await?["contract"]["verified"].as_bool().unwrap_or(false))
            }
            Backend::Blockscout => {
                Ok(self.get(&format!("/api/v2/smart-contracts/{address}")).await?["is_verified"].as_bool().unwrap_or(false))
            }
            Backend::ChainOnly => Err(self.unsupported("contract verification")),
        }
    }

    // ------------------------------------------------------------ address lookups

    /// Token and NFT balances for an address.
    pub async fn holdings(&self, address: &str) -> Result<Vec<Holding>> {
        match self.backend {
            Backend::Quai => {
                let mut out = Vec::new();
                let mut offset = 0;
                loop {
                    let v = self.get(&format!("/api/address/{}/token-balances?limit=100&offset={offset}", address.to_lowercase())).await?;
                    let page = parse_quai_holdings(&v, self);
                    let n = v["items"].as_array().map_or(0, Vec::len);
                    out.extend(page);
                    let total = num_u64(&v["total"]).unwrap_or(0) as usize;
                    offset += n;
                    if n == 0 || offset >= total || offset >= 500 {
                        break;
                    }
                }
                Ok(out)
            }
            Backend::Blockscout => {
                Ok(parse_blockscout_holdings(&self.get(&format!("/api/v2/addresses/{address}/token-balances")).await?, self))
            }
            Backend::ChainOnly => Err(self.unsupported("token discovery")),
        }
    }

    /// Recent token and NFT transfers (newest first), up to `max` rows.
    pub async fn token_transfers(&self, address: &str, max: usize) -> Result<Vec<TokenTransfer>> {
        match self.backend {
            Backend::Quai => {
                let mut out = Vec::new();
                let mut cursor: Option<String> = None;
                while out.len() < max {
                    let limit = (max - out.len()).min(200);
                    let path = match &cursor {
                        Some(c) => format!("/api/address/{}/token-transfers-v2?limit={limit}&cursor={c}", address.to_lowercase()),
                        None => format!("/api/address/{}/token-transfers-v2?limit={limit}", address.to_lowercase()),
                    };
                    let v = self.get(&path).await?;
                    out.extend(parse_quai_transfers(&v));
                    cursor = v["nextCursor"]
                        .as_str()
                        .filter(|c| c.chars().all(|ch| ch.is_ascii_alphanumeric() || "-_=".contains(ch)))
                        .map(str::to_string);
                    if !v["hasMore"].as_bool().unwrap_or(false) || cursor.is_none() {
                        break;
                    }
                }
                Ok(out)
            }
            Backend::Blockscout => {
                Ok(parse_blockscout_transfers(&self.get(&format!("/api/v2/addresses/{address}/token-transfers")).await?)
                    .into_iter()
                    .take(max)
                    .collect())
            }
            Backend::ChainOnly => Err(self.unsupported("token transfers")),
        }
    }

    /// Transfers newest first, stopping once a page reaches `overlap_block` (history already
    /// held, minus a re-read margin) or after `max` rows. Quai backend only.
    pub async fn token_transfers_since(&self, address: &str, overlap_block: Option<u64>, max: usize) -> Result<Vec<TokenTransfer>> {
        if self.backend != Backend::Quai {
            return self.token_transfers(address, max).await;
        }
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        while out.len() < max {
            let limit = (max - out.len()).min(200);
            let path = match &cursor {
                Some(c) => format!("/api/address/{}/token-transfers-v2?limit={limit}&cursor={c}", address.to_lowercase()),
                None => format!("/api/address/{}/token-transfers-v2?limit={limit}", address.to_lowercase()),
            };
            let v = self.get(&path).await?;
            let page = parse_quai_transfers(&v);
            let reached = overlap_block.is_some_and(|b| page.iter().any(|t| t.block < b));
            out.extend(page);
            cursor = v["nextCursor"]
                .as_str()
                .filter(|c| c.chars().all(|ch| ch.is_ascii_alphanumeric() || "-_=".contains(ch)))
                .map(str::to_string);
            if reached || !v["hasMore"].as_bool().unwrap_or(false) || cursor.is_none() {
                break;
            }
        }
        Ok(out)
    }

    /// Native balance changes, newest first.
    pub async fn balance_history(&self, address: &str) -> Result<Vec<BalanceChange>> {
        match self.backend {
            Backend::Quai => {
                Ok(parse_balance_history(&self.get(&format!("/api/address/{}/balance-history", address.to_lowercase())).await?))
            }
            _ => Err(self.unsupported("balance history")),
        }
    }

    /// Lockups summary for the locks cross-check.
    pub async fn lockups(&self, address: &str) -> Result<LockupSummary> {
        match self.backend {
            Backend::Quai => {
                let v = self.get(&format!("/api/address/{}/lockups/summary?v=2", address.to_lowercase())).await?;
                Ok(LockupSummary { total: num_u64(&v["total"]).unwrap_or(0), head: num_u64(&v["head"]).unwrap_or(0) })
            }
            _ => Err(self.unsupported("lockups")),
        }
    }

    /// NFTs currently held according to the indexer: (contract, token id, kind, quantity).
    /// explorer.qu.ai does not return ids with balances (and ignores an `owner=` filter), so
    /// ids come from replaying the address's transfers. Callers verify ownership on-chain.
    pub async fn nft_candidates(&self, address: &str) -> Result<Vec<(String, String, TokenKind, U256)>> {
        match self.backend {
            Backend::Quai => {
                let transfers = self.token_transfers(address, 10_000).await?;
                Ok(replay_nft_holdings(address, &transfers))
            }
            Backend::Blockscout => {
                let v = self.get(&format!("/api/v2/addresses/{address}/nft?type=ERC-721,ERC-1155")).await?;
                Ok(parse_blockscout_nfts(&v, self)
                    .into_iter()
                    .map(|n| {
                        (
                            n.contract.clone(),
                            n.token_id.clone(),
                            n.kind.unwrap_or(TokenKind::Erc721),
                            integer(&Value::String(n.quantity.clone())).unwrap_or(U256::from(1)),
                        )
                    })
                    .collect())
            }
            Backend::ChainOnly => Err(self.unsupported("NFT discovery")),
        }
    }

    /// NFTs an address holds with their ids and metadata, from the Blockscout-compatible
    /// `/api/v2/addresses/{a}/nft` (explorer.qu.ai serves it too). At most 500 items.
    pub async fn owned_nfts(&self, address: &str) -> Result<Vec<NftItem>> {
        if self.backend == Backend::ChainOnly {
            return Err(self.unsupported("NFT discovery"));
        }
        let address = address.to_lowercase();
        if !(address.starts_with("0x") && address.len() == 42 && address[2..].chars().all(|c| c.is_ascii_hexdigit())) {
            return Err(CoreError::Invalid("address".into()));
        }
        let mut out = Vec::new();
        let mut query = String::new();
        loop {
            let v = self.get(&format!("/api/v2/addresses/{address}/nft?type=ERC-721,ERC-1155{query}")).await?;
            out.extend(parse_blockscout_nfts(&v, self));
            // The next page is named by the fields of `next_page_params`.
            let params: Vec<String> = v["next_page_params"]
                .as_object()
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| {
                            let value = match v {
                                Value::String(t) => t.clone(),
                                Value::Number(n) => n.to_string(),
                                _ => return None,
                            };
                            let safe = |t: &str| t.chars().all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c));
                            (safe(k) && safe(&value)).then(|| format!("&{k}={value}"))
                        })
                        .collect()
                })
                .unwrap_or_default();
            if params.is_empty() || out.len() >= 500 {
                break;
            }
            query = params.concat();
        }
        Ok(out)
    }

    /// One NFT with metadata.
    pub async fn nft(&self, contract: &str, token_id: &str) -> Result<NftItem> {
        if !token_id.chars().all(|c| c.is_ascii_digit()) || token_id.is_empty() || token_id.len() > 78 {
            return Err(CoreError::Invalid("token id must be a decimal integer".into()));
        }
        match self.backend {
            Backend::Quai => {
                let v = self.get(&format!("/api/token/{}/instance/{token_id}", contract.to_lowercase())).await?;
                let mut item = parse_quai_instance(&v["instance"], self);
                item.collection = s(&v["token"]["name"]);
                item.kind = TokenKind::parse(&s(&v["token"]["type"]));
                if item.contract.is_empty() {
                    item.contract = contract.to_lowercase();
                }
                Ok(item)
            }
            Backend::Blockscout => {
                let v = self.get(&format!("/api/v2/tokens/{contract}/instances/{token_id}")).await?;
                Ok(parse_blockscout_instance(&v, self))
            }
            Backend::ChainOnly => Err(self.unsupported("NFT metadata")),
        }
    }

    /// Items of a collection (first page).
    pub async fn collection_items(&self, contract: &str, limit: usize) -> Result<Vec<NftItem>> {
        match self.backend {
            Backend::Quai => {
                let v = self.get(&format!("/api/token/{}/instances?limit={}", contract.to_lowercase(), limit.min(100))).await?;
                Ok(v["items"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|i| {
                                let mut item = parse_quai_instance(i, self);
                                item.contract = contract.to_lowercase();
                                item
                            })
                            .collect()
                    })
                    .unwrap_or_default())
            }
            Backend::Blockscout => {
                let v = self.get(&format!("/api/v2/tokens/{contract}/instances")).await?;
                Ok(v["items"]
                    .as_array()
                    .map(|a| a.iter().take(limit).map(|i| parse_blockscout_instance(i, self)).collect())
                    .unwrap_or_default())
            }
            Backend::ChainOnly => Err(self.unsupported("collection items")),
        }
    }

    /// Collections directory (search by name when `query` is set).
    pub async fn collections(&self, query: Option<&str>, limit: usize) -> Result<Vec<Collection>> {
        match self.backend {
            Backend::Quai => {
                let q: String = query
                    .unwrap_or("")
                    .chars()
                    .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-')
                    .take(40)
                    .collect::<String>()
                    .replace(' ', "%20");
                let path = if q.is_empty() {
                    format!("/api/nft/collections?limit={}", limit.min(100))
                } else {
                    format!("/api/nft/collections?limit={}&q={q}", limit.min(100))
                };
                Ok(parse_collections(&self.get(&path).await?, self))
            }
            _ => Err(self.unsupported("the collections directory")),
        }
    }
}

// ---------------------------------------------------------------- parsers (pure, fixture-tested)

/// `/api/convert/quote`.
pub fn parse_convert_steps(v: &Value) -> ConversionSteps {
    let t = &v["steps"];
    let num = |x: &Value| match x {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => "0".into(),
    };
    ConversionSteps {
        direction: s(&v["direction"]),
        value_in_quai: num(&t["amountInQuai"]),
        flow_amount: num(&t["flowAmount"]),
        after_flow_discount: num(&t["cubicDiscounted"]),
        kquai_applied: t["kQuaiDiscountApplied"].as_bool().unwrap_or(false),
        kquai_discount: num(&t["kQuaiDiscount"]),
        floored_at_10pct: t["flooredAt10Pct"].as_bool().unwrap_or(false),
        value_out_quai_terms: num(&t["valueOutQuaiTerms"]),
        value_out: num(&t["valueOut"]),
    }
}

/// `/api/stats/tvl`.
pub fn parse_tvl(v: &Value) -> PoolBoard {
    let pools = v["pools"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|p| p["address"].as_str().is_some_and(|x| x.starts_with("0x")))
                .map(|p| PoolStats {
                    address: s(&p["address"]).to_lowercase(),
                    name: clean_text(&s(&p["name"])),
                    tvl_usd: num_f64(&p["tvlUsd"]),
                    volume_24h_usd: num_f64(&p["volume24hUsd"]),
                })
                .collect()
        })
        .unwrap_or_default();
    let observed_at = v["freshness"]["observedAt"].as_str().or(v["current"]["observedAt"].as_str()).and_then(parse_timestamp).unwrap_or(0);
    PoolBoard { pools, observed_at, stale: v["stale"].as_bool().unwrap_or(false) }
}

/// `/api/price/current`.
pub fn parse_prices(v: &Value) -> PriceBoard {
    let quai_at = v["quai"]["takenAt"].as_str().and_then(parse_timestamp).unwrap_or(0);
    let qi_at = v["qi"]["takenAt"].as_str().and_then(parse_timestamp).unwrap_or(0);
    PriceBoard {
        quai_usd: num_f64(&v["quai"]["usd"]).filter(|p| *p > 0.0),
        quai_source: s(&v["quai"]["source"]),
        qi_usd: num_f64(&v["qi"]["usd"]).filter(|p| *p > 0.0),
        qi_source: s(&v["qi"]["source"]),
        taken_at: quai_at.max(qi_at),
        observed_at: now(),
    }
}

/// `/api/stats/assets?kind=token`.
pub fn parse_stats_assets(v: &Value, ex: &Explorer) -> Vec<TokenMarket> {
    v["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|i| {
                    let address = s(&i["contractAddress"]).to_lowercase();
                    if address.is_empty() {
                        return None;
                    }
                    Some(TokenMarket {
                        address,
                        symbol: s(&i["symbol"]),
                        name: s(&i["name"]),
                        price_usd: num_f64(&i["priceUsd"]).filter(|p| *p > 0.0),
                        price_source: s(&i["provenance"]["priceUsd"]),
                        holders: num_u64(&i["holders"]),
                        cap_growth_24h: num_f64(&i["marketCapGrowth24h"]),
                        icon_url: opt_s(&i["iconUrl"]).map(|u| ex.absolute(&u)),
                        price_at: i["freshness"]["priceUsdAt"].as_str().and_then(parse_timestamp).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_token_info(t: &Value, ex: &Explorer) -> TokenInfo {
    TokenInfo {
        address: s(&t["contract_address"]).to_lowercase(),
        name: s(&t["name"]),
        symbol: s(&t["symbol"]),
        decimals: num_u64(&t["decimals"]).and_then(|d| u8::try_from(d).ok()),
        holders: num_u64(&t["holder_count"]),
        kind: TokenKind::parse(&s(&t["type"])),
        icon_url: opt_s(&t["icon_url"]).map(|u| ex.absolute(&u)),
    }
}

fn parse_blockscout_token(t: &Value, ex: &Explorer) -> TokenInfo {
    TokenInfo {
        address: s(&t["address"]).to_lowercase(),
        name: s(&t["name"]),
        symbol: s(&t["symbol"]),
        decimals: num_u64(&t["decimals"]).and_then(|d| u8::try_from(d).ok()),
        holders: num_u64(&t["holders"]).or_else(|| num_u64(&t["holders_count"])),
        kind: TokenKind::parse(&s(&t["type"])),
        icon_url: opt_s(&t["icon_url"]).map(|u| ex.absolute(&u)),
    }
}

/// `/api/address/{a}/token-balances`.
pub fn parse_quai_holdings(v: &Value, ex: &Explorer) -> Vec<Holding> {
    v["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|i| {
                    let t = &i["token"];
                    Some(Holding {
                        token: s(&i["token_address"]).to_lowercase(),
                        symbol: s(&t["symbol"]),
                        name: s(&t["name"]),
                        decimals: num_u64(&t["decimals"]).and_then(|d| u8::try_from(d).ok()),
                        kind: TokenKind::parse(&s(&t["type"]))?,
                        balance: integer(&i["balance"])?,
                        icon_url: opt_s(&t["icon_url"]).map(|u| ex.absolute(&u)),
                    })
                })
                .filter(|h| !h.balance.is_zero())
                .collect()
        })
        .unwrap_or_default()
}

/// Blockscout `/api/v2/addresses/{a}/token-balances`.
pub fn parse_blockscout_holdings(v: &Value, ex: &Explorer) -> Vec<Holding> {
    v.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|i| {
                    let t = &i["token"];
                    Some(Holding {
                        token: s(&t["address"]).to_lowercase(),
                        symbol: s(&t["symbol"]),
                        name: s(&t["name"]),
                        decimals: num_u64(&t["decimals"]).and_then(|d| u8::try_from(d).ok()),
                        kind: TokenKind::parse(&s(&t["type"]))?,
                        balance: integer(&i["value"])?,
                        icon_url: opt_s(&t["icon_url"]).map(|u| ex.absolute(&u)),
                    })
                })
                .filter(|h| !h.balance.is_zero())
                .collect()
        })
        .unwrap_or_default()
}

/// `/api/address/{a}/token-transfers-v2`.
pub fn parse_quai_transfers(v: &Value) -> Vec<TokenTransfer> {
    v["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|i| {
                    Some(TokenTransfer {
                        tx_hash: s(&i["tx_hash"]).to_lowercase(),
                        log_index: num_u64(&i["log_index"]).unwrap_or(0),
                        block: num_u64(&i["block_height"]).unwrap_or(0),
                        timestamp: i["timestamp"].as_str().and_then(parse_timestamp).unwrap_or(0),
                        from: lower_address(&i["from_addr"]),
                        to: lower_address(&i["to_addr"]),
                        value: integer(&i["value"]).unwrap_or(U256::from(1)),
                        token_id: opt_s(&i["token_id"]),
                        token: s(&i["token_address"]).to_lowercase(),
                        symbol: s(&i["token_symbol"]),
                        name: s(&i["token_name"]),
                        decimals: num_u64(&i["token_decimals"]).and_then(|d| u8::try_from(d).ok()),
                        kind: TokenKind::parse(&s(&i["token_type"]))?,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Blockscout `/api/v2/addresses/{a}/token-transfers`.
pub fn parse_blockscout_transfers(v: &Value) -> Vec<TokenTransfer> {
    v["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|i| {
                    let t = &i["token"];
                    let total = &i["total"];
                    Some(TokenTransfer {
                        tx_hash: s(&i["transaction_hash"]).to_lowercase(),
                        log_index: num_u64(&i["log_index"]).unwrap_or(0),
                        block: num_u64(&i["block_number"]).unwrap_or(0),
                        timestamp: i["timestamp"].as_str().and_then(parse_timestamp).unwrap_or(0),
                        from: lower_address(&i["from"]),
                        to: lower_address(&i["to"]),
                        value: integer(&total["value"]).unwrap_or(U256::from(1)),
                        token_id: opt_s(&total["token_id"]),
                        token: s(&t["address"]).to_lowercase(),
                        symbol: s(&t["symbol"]),
                        name: s(&t["name"]),
                        decimals: num_u64(&t["decimals"]).and_then(|d| u8::try_from(d).ok()),
                        kind: TokenKind::parse(&s(&t["type"]))?,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `/api/address/{a}/balance-history`.
pub fn parse_balance_history(v: &Value) -> Vec<BalanceChange> {
    v["history"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|i| i["is_canonical"].as_bool().unwrap_or(true))
                .filter_map(|i| {
                    let delta_text = s(&i["delta"]);
                    let (neg, digits) = delta_text.strip_prefix('-').map_or((false, delta_text.as_str()), |d| (true, d));
                    let magnitude = parse_indexer_integer(digits)?;
                    Some(BalanceChange {
                        timestamp: i["timestamp"].as_str().and_then(parse_timestamp)?,
                        block: num_u64(&i["block_height"]).unwrap_or(0),
                        delta: if neg && !magnitude.is_zero() { format!("-{magnitude}") } else { magnitude.to_string() },
                        coin: s(&i["coin_type"]),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Net NFT holdings from transfers (ERC-721 ids with a net inbound transfer; ERC-1155 net quantity).
pub fn replay_nft_holdings(address: &str, transfers: &[TokenTransfer]) -> Vec<(String, String, TokenKind, U256)> {
    use std::collections::BTreeMap;
    let me = address.to_lowercase();
    let mut ordered: Vec<&TokenTransfer> = transfers.iter().filter(|t| t.kind != TokenKind::Erc20 && t.token_id.is_some()).collect();
    ordered.sort_by_key(|t| (t.block, t.log_index));
    let mut held: BTreeMap<(String, String), (TokenKind, i128)> = BTreeMap::new();
    for t in ordered {
        let key = (t.token.clone(), t.token_id.clone().unwrap_or_default());
        let qty = i128::try_from(u128::try_from(t.value).unwrap_or(1)).unwrap_or(1);
        let entry = held.entry(key).or_insert((t.kind, 0));
        if t.kind == TokenKind::Erc721 {
            if t.to == me {
                entry.1 = 1;
            } else if t.from == me {
                entry.1 = 0;
            }
        } else {
            if t.to == me {
                entry.1 += qty;
            }
            if t.from == me {
                entry.1 -= qty;
            }
        }
    }
    held.into_iter().filter(|(_, (_, q))| *q > 0).map(|((c, id), (k, q))| (c, id, k, U256::from(q as u128))).collect()
}

fn traits(metadata: &Value) -> Vec<(String, String)> {
    metadata["attributes"]
        .as_array()
        .map(|a| {
            a.iter()
                .take(40)
                .filter_map(|t| {
                    let key = s(&t["trait_type"]);
                    let value = s(&t["value"]);
                    (!key.is_empty() || !value.is_empty()).then(|| (clip(key, 40), clip(value, 60)))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn clip(text: String, max: usize) -> String {
    if text.chars().count() > max { text.chars().take(max).collect::<String>() + "…" } else { text }
}

/// explorer.qu.ai instance object (from `/instance/{id}` or `/instances`).
pub fn parse_quai_instance(i: &Value, ex: &Explorer) -> NftItem {
    let meta = &i["metadata_json"];
    let image = opt_s(&i["media_url"]).map(|u| ex.absolute(&u)).or_else(|| opt_s(&meta["image"]));
    NftItem {
        contract: s(&i["token_address"]).to_lowercase(),
        token_id: s(&i["token_id"]),
        kind: None,
        name: clip(opt_s(&meta["name"]).unwrap_or_else(|| format!("#{}", s(&i["token_id"]))), 80),
        collection: String::new(),
        description: clip(s(&meta["description"]), 600),
        image,
        traits: traits(meta),
        owner: opt_s(&i["owner_address"]).map(|o| o.to_lowercase()),
        quantity: opt_s(&i["quantity"]).unwrap_or_else(|| "1".into()),
    }
}

fn parse_blockscout_instance(i: &Value, ex: &Explorer) -> NftItem {
    let meta = &i["metadata"];
    NftItem {
        contract: s(&i["token"]["address"]).to_lowercase(),
        token_id: s(&i["id"]),
        kind: TokenKind::parse(&s(&i["token"]["type"])),
        name: clip(opt_s(&meta["name"]).unwrap_or_else(|| format!("#{}", s(&i["id"]))), 80),
        collection: s(&i["token"]["name"]),
        description: clip(s(&meta["description"]), 600),
        image: opt_s(&i["image_url"]).or_else(|| opt_s(&meta["image"])).map(|u| ex.absolute(&u)),
        traits: traits(meta),
        owner: i["owner"]["hash"].as_str().map(str::to_lowercase),
        quantity: opt_s(&i["value"]).unwrap_or_else(|| "1".into()),
    }
}

/// Blockscout `/api/v2/addresses/{a}/nft`.
pub fn parse_blockscout_nfts(v: &Value, ex: &Explorer) -> Vec<NftItem> {
    v["items"].as_array().map(|a| a.iter().map(|i| parse_blockscout_instance(i, ex)).collect()).unwrap_or_default()
}

/// `/api/nft/collections`.
pub fn parse_collections(v: &Value, ex: &Explorer) -> Vec<Collection> {
    v["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|i| Collection {
                    address: s(&i["contract_address"]).to_lowercase(),
                    name: clip(s(&i["name"]), 60),
                    symbol: clip(s(&i["symbol"]), 16),
                    kind: TokenKind::parse(&s(&i["type"])),
                    holders: num_u64(&i["holder_count"]),
                    preview: opt_s(&i["preview_media_url"]).map(|u| ex.absolute(&u)),
                    floor_quai: num_f64(&i["floor_price_quai"]).filter(|f| *f > 0.0),
                    floor_usd: num_f64(&i["floor_price_usd"]).filter(|f| *f > 0.0),
                })
                .filter(|c| !c.address.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn display_text_drops_invisible_spoofing_and_line_separators_and_stays_bounded() {
        assert_eq!(super::clean("US\u{00ad}\u{061c}\u{2060}\u{feff}DT\u{2028}\u{2029}\u{1b}", 32), "USDT");
        assert_eq!(super::clean("αβγδε", 3), "αβγ", "Unicode letters remain visible and character-boundary safe");
    }

    use super::*;

    /// What would make a terminal draw a name wider than the layout measured it is removed; plain
    /// emoji, which both count as two cells, stay.
    #[test]
    fn untrusted_text_keeps_its_measured_width() {
        assert_eq!(clean("Love\u{2764}\u{FE0F}", 64), "Love\u{2764}");
        assert_eq!(clean("GM \u{1F44B}\u{1F3FD}", 64), "GM \u{1F44B}", "the skin tone goes, the hand stays");
        assert_eq!(clean("\u{1F1FA}\u{1F1F8} DAO", 64), " DAO", "flags are two cells on screen and one each in the table");
        assert_eq!(clean("1\u{FE0F}\u{20E3}", 64), "1");
        assert_eq!(clean("tag\u{E0067}\u{E0062}\u{E007F}", 64), "tag");
        assert_eq!(clean("Pepe \u{1F438}", 64), "Pepe \u{1F438}", "plain emoji stay");
    }

    fn fixture(name: &str) -> Value {
        let text = match name {
            "price" => include_str!("fixtures/quai_price_current.json"),
            "assets" => include_str!("fixtures/quai_stats_assets.json"),
            "balances" => include_str!("fixtures/quai_token_balances.json"),
            "transfers" => include_str!("fixtures/quai_token_transfers.json"),
            "history" => include_str!("fixtures/quai_balance_history.json"),
            "collections" => include_str!("fixtures/quai_nft_collections.json"),
            "instance" => include_str!("fixtures/quai_nft_instance.json"),
            "instances" => include_str!("fixtures/quai_nft_instances.json"),
            "bs_balances" => include_str!("fixtures/blockscout_token_balances.json"),
            "bs_transfers" => include_str!("fixtures/blockscout_token_transfers.json"),
            "bs_nft" => include_str!("fixtures/blockscout_nft.json"),
            "tvl" => include_str!("fixtures/quai_stats_tvl.json"),
            "convert" => include_str!("fixtures/quai_convert_quote.json"),
            other => panic!("no fixture {other}"),
        };
        serde_json::from_str(text).unwrap()
    }

    fn quai() -> Explorer {
        Explorer { backend: Backend::Quai, base: "https://explorer.qu.ai".into() }
    }

    #[test]
    fn conversion_steps() {
        let c = parse_convert_steps(&fixture("convert"));
        assert_eq!(c.direction, "qi-to-quai");
        assert_eq!(c.value_out, "218009575127946538204");
        assert!(c.kquai_applied && !c.floored_at_10pct);
        let lines = c.lines();
        assert_eq!(lines.first().unwrap(), "input worth 912.6281 QUAI");
        assert_eq!(lines.last().unwrap(), "out 218.0095 QUAI");
        let floored = parse_convert_steps(
            &serde_json::json!({"direction": "quai-to-qi", "steps": {"amountInQuai": "1000000000000000000000", "flooredAt10Pct": true, "valueOutQuaiTerms": "100000000000000000000", "valueOut": "1096"}}),
        );
        assert!(floored.lines().iter().any(|l| l.contains("10% of input")));
        assert_eq!(floored.lines().last().unwrap(), "out 1.096 Qi");
    }

    #[test]
    fn pool_liquidity() {
        let b = parse_tvl(&fixture("tvl"));
        assert!(!b.pools.is_empty());
        let p = b.pools.iter().find(|p| p.name == "USDT/WQUAI").unwrap();
        assert_eq!(p.address, "0x0021f5cc862ebb0252ba209266f2fabbc7592e83");
        assert!((p.tvl_usd.unwrap() - 3261.584916).abs() < 1e-6);
        assert_eq!(b.observed_at, parse_timestamp("2026-09-15T11:59:14.843Z").unwrap());
        assert!(!b.stale);
        assert!(parse_tvl(&serde_json::json!({})).pools.is_empty());
    }

    #[test]
    fn timestamps() {
        assert_eq!(parse_timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_timestamp("2026-09-15T11:49:15.299Z"), Some(1_789_472_955));
        assert_eq!(parse_timestamp("2026-01-27T23:58:03.000000Z"), Some(1_769_558_283));
        assert_eq!(parse_timestamp("garbage"), None);
    }

    #[test]
    fn prices_and_markets() {
        let p = parse_prices(&fixture("price"));
        assert!(p.quai_usd.unwrap() > 0.0 && p.qi_usd.unwrap() > 0.0);
        assert_eq!(p.qi_source, "derived:protocol-rate");
        let m = parse_stats_assets(&fixture("assets"), &quai());
        let wqi = m.iter().find(|t| t.symbol == "WQI").expect("WQI in fixture");
        assert_eq!(wqi.address, "0x002b2596ecf05c93a31ff916e8b456df6c77c750");
        assert!(wqi.price_usd.is_some());
        assert_eq!(wqi.icon_url.as_deref(), Some("https://explorer.qu.ai/token-icons/wrapped-qi.svg"));
    }

    #[test]
    fn holdings_both_backends() {
        let h = parse_quai_holdings(&fixture("balances"), &quai());
        assert!(h.iter().any(|x| x.kind == TokenKind::Erc721 && x.balance == U256::from(20)));
        let fly = h.iter().find(|x| x.symbol == "FLY").unwrap();
        assert_eq!(fly.balance, U256::from(10u128.pow(22)));
        let bs = Explorer { backend: Backend::Blockscout, base: "https://orchard.quaiscan.io".into() };
        let b = parse_blockscout_holdings(&fixture("bs_balances"), &bs);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].decimals, Some(18));
        assert_eq!(b[1].kind, TokenKind::Erc721);
    }

    #[test]
    fn transfers_and_nft_replay() {
        let t = parse_quai_transfers(&fixture("transfers"));
        assert!(!t.is_empty());
        assert!(t.iter().all(|x| x.timestamp > 0 && x.tx_hash.starts_with("0x")));
        let bt = parse_blockscout_transfers(&fixture("bs_transfers"));
        assert!(!bt.is_empty() && bt[0].timestamp > 0);
        let me = "0x00aa";
        let mk = |from: &str, to: &str, id: &str, block: u64, kind: TokenKind, value: u64| TokenTransfer {
            tx_hash: "0x".into(),
            log_index: 0,
            block,
            timestamp: 0,
            from: from.into(),
            to: to.into(),
            value: U256::from(value),
            token_id: Some(id.into()),
            token: "0xc0".into(),
            symbol: "C".into(),
            name: "C".into(),
            decimals: None,
            kind,
        };
        let transfers = vec![
            mk("0x0", me, "1", 1, TokenKind::Erc721, 1),
            mk("0x0", me, "2", 2, TokenKind::Erc721, 1),
            mk(me, "0xbb", "1", 3, TokenKind::Erc721, 1),
            mk("0x0", me, "9", 4, TokenKind::Erc1155, 5),
            mk(me, "0xbb", "9", 5, TokenKind::Erc1155, 2),
        ];
        let held = replay_nft_holdings(me, &transfers);
        assert_eq!(held.len(), 2);
        assert_eq!(held[0].1, "2");
        assert_eq!((held[1].1.as_str(), held[1].3), ("9", U256::from(3)));
    }

    #[test]
    fn history_collections_instances() {
        let h = parse_balance_history(&fixture("history"));
        assert!(!h.is_empty() && h[0].delta.starts_with('-'));
        let c = parse_collections(&fixture("collections"), &quai());
        assert!(!c.is_empty() && c[0].preview.as_deref().is_some_and(|p| p.starts_with("https://explorer.qu.ai/api/nft-media/")));
        let v = fixture("instance");
        let i = parse_quai_instance(&v["instance"], &quai());
        assert_eq!(i.name, "Quai Miners #1");
        assert!(i.traits.iter().any(|(k, v)| k == "Background" && v == "Quarry"));
        assert!(i.image.unwrap().starts_with("https://explorer.qu.ai/api/nft-media/"));
        let bs = Explorer { backend: Backend::Blockscout, base: "https://orchard.quaiscan.io".into() };
        let n = parse_blockscout_nfts(&fixture("bs_nft"), &bs);
        assert!(!n.is_empty() && n[0].kind == Some(TokenKind::Erc721));
        let items = fixture("instances");
        assert_eq!(items["items"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn explorer_v2_nft_listing() {
        // Shape of explorer.qu.ai `/api/v2/addresses/{a}/nft` (Blockscout-compatible).
        let v = serde_json::json!({"items": [{
            "id": "207",
            "token": {"address": "0x00378F533BC9854274Bf0D33F0D34Ab00402E86a", "name": "Caelestis Praetor", "type": "ERC-721"},
            "token_type": "ERC-721",
            "value": "1",
            "owner": {"hash": "0x0000000000000000000000000000000000000Abc"},
            "image_url": "https://explorer.qu.ai/api/nft-media/8d8f",
            "metadata": {"name": "Caelestis Praetor #207", "image": "ipfs://bafy"}
        }], "next_page_params": null});
        let items = parse_blockscout_nfts(&v, &quai());
        assert_eq!(items.len(), 1);
        let i = &items[0];
        assert_eq!((i.contract.as_str(), i.token_id.as_str()), ("0x00378f533bc9854274bf0d33f0d34ab00402e86a", "207"));
        assert_eq!(i.kind, Some(TokenKind::Erc721));
        assert_eq!(i.name, "Caelestis Praetor #207");
        assert_eq!(i.image.as_deref(), Some("https://explorer.qu.ai/api/nft-media/8d8f"));
        assert_eq!(i.collection, "Caelestis Praetor");
    }
}
