//! NFTs: holdings verified on-chain, transfers, the Bazarr listings indexer and buying Zora V3
//! asks. The indexer only suggests; before any transfer or purchase the wallet re-reads
//! ownership and the ask on-chain, and the SDK simulates the exact call.

use crate::journal::OpKind;
use crate::amount::{self, QUAI_DECIMALS};
use crate::appdb::AppDb;
use crate::chain::{addr, interface, is_zero_address};
use crate::data::{DataCtx, ERC1155_ABI, NFT_ABI, READ_CALLER, with_access_list};
use crate::error::{CoreError, Result};
use crate::explorer::{NftItem, TokenKind, clean_text};
use crate::http;
use crate::network::{NetworkProfile, Node};
use crate::session::Session;
use crate::tx::{AccountRequest, Review, field};
use quai_sdk::contracts::{Contract, Erc20};
use quai_sdk::{BlockTag, QuaiAddress, U256};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Zora V3 Asks v1.1 ABI subset.
pub const ASKS_ABI: &[&str] = &[
    "function askForNFT(address tokenContract, uint256 tokenId) view returns (address seller, address sellerFundsRecipient, address askCurrency, uint16 findersFeeBps, uint256 askPrice)",
    "function fillAsk(address tokenContract, uint256 tokenId, address fillCurrency, uint256 fillAmount, address finder) payable",
    "function erc721TransferHelper() view returns (address)",
    "function erc20TransferHelper() view returns (address)",
    "function createAsk(address _tokenContract, uint256 _tokenId, uint256 _askPrice, address _askCurrency, address _sellerFundsRecipient, uint16 _findersFeeBps)",
    "function setAskPrice(address _tokenContract, uint256 _tokenId, uint256 _askPrice, address _askCurrency)",
    "function cancelAsk(address _tokenContract, uint256 _tokenId)",
];

/// Zora module manager ABI subset.
pub const MODULE_MANAGER_ABI: &[&str] = &[
    "function isModuleApproved(address user, address module) view returns (bool)",
    "function setApprovalForModule(address module, bool approved)",
];

/// The zero address as the ABI encoder expects it.
pub const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

/// A marketplace listing from the Bazarr indexer (untrusted until re-checked on-chain).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Listing {
    /// Collection contract (lowercase).
    pub contract: String,
    /// Token id.
    pub token_id: String,
    /// Seller (lowercase).
    pub seller: String,
    /// Price in base units of `currency`.
    pub price: String,
    /// Currency contract (zero address = native QUAI).
    pub currency: String,
    /// `zora` or `seaport`.
    pub protocol: String,
    /// Quantity offered.
    pub quantity: String,
    /// Listing time (unix seconds).
    pub created_at: u64,
    /// Item name, when the indexer has it.
    pub name: Option<String>,
    /// Image reference, when the indexer has it.
    pub image: Option<String>,
}

impl Listing {
    /// Priced in native QUAI.
    pub fn is_native(&self) -> bool {
        self.currency.trim_start_matches("0x").chars().all(|c| c == '0')
    }
    /// Price as U256.
    pub fn price_amount(&self) -> U256 {
        U256::from_str_radix(&self.price, 10).unwrap_or_default()
    }
    /// Human price (native QUAI or raw units with the currency's short address).
    pub fn price_text(&self) -> String {
        if self.is_native() {
            format!("{} QUAI", amount::group_thousands(&amount::format_amount_short(self.price_amount(), QUAI_DECIMALS, 4)))
        } else {
            format!("{} of {}", amount::format_amount_short(self.price_amount(), 18, 4), crate::session::short_address(&self.currency))
        }
    }
    /// Price with the currency named when it is one of the network's known tokens
    /// (WQI, WQUAI, USDT); other currencies keep their contract address.
    pub fn price_text_on(&self, network: &NetworkProfile) -> String {
        match known_currency(network, &self.currency) {
            Some((symbol, decimals)) if !self.is_native() => {
                format!("{} {symbol}", amount::group_thousands(&amount::format_amount_short(self.price_amount(), decimals, 4)))
            }
            _ => self.price_text(),
        }
    }
    /// Only Zora asks can be bought in-wallet.
    pub fn buyable(&self) -> bool {
        self.protocol == "zora"
    }
}

/// Symbol and decimals of a listing currency the network profile knows.
pub fn known_currency(network: &NetworkProfile, currency: &str) -> Option<(&'static str, u8)> {
    let is = |a: Option<&String>| a.is_some_and(|a| a.eq_ignore_ascii_case(currency));
    if is(network.wqi.as_ref()) {
        Some(("WQI", 18))
    } else if is(network.wquai.as_ref()) {
        Some(("WQUAI", 18))
    } else if is(network.ecosystem.usdt.as_ref().map(|u| &u.address)) {
        Some(("USDT", 6))
    } else {
        None
    }
}

/// Parse `/listings` from the Bazarr indexer.
pub fn parse_listings(v: &Value) -> Vec<Listing> {
    let text = |x: &Value| x.as_str().map(clean_text).unwrap_or_default();
    v["listings"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|l| {
                    let token_id = text(&l["token_id"]);
                    let price = text(&l["price"]);
                    if !token_id.chars().all(|c| c.is_ascii_digit()) || token_id.is_empty() || U256::from_str_radix(&price, 10).is_err() {
                        return None;
                    }
                    let image = l["meta_image_url"]
                        .as_str()
                        .filter(|u| u.starts_with("https://") || u.starts_with("ipfs://") || u.starts_with("data:image/"))
                        .map(str::to_string);
                    Some(Listing {
                        contract: text(&l["contract"]).to_lowercase(),
                        token_id,
                        seller: text(&l["seller"]).to_lowercase(),
                        price,
                        currency: text(&l["currency"]).to_lowercase(),
                        protocol: text(&l["protocol"]).to_lowercase(),
                        quantity: l["quantity_remaining"].as_str().or_else(|| l["quantity"].as_str()).unwrap_or("1").to_string(),
                        created_at: l["created_at"].as_str().and_then(crate::explorer::parse_timestamp).unwrap_or(0),
                        name: l["meta_name"].as_str().map(clean_text).filter(|n| !n.is_empty()).map(|n| n.chars().take(80).collect()),
                        image,
                    })
                })
                .filter(|l| l.contract.starts_with("0x") && !l.seller.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// A collection's marketplace standing, from the Bazarr indexer. Everything here is the
/// indexer's own bookkeeping of the marketplace contracts; names and links are untrusted text.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct CollectionStats {
    /// Contract (lowercase).
    pub address: String,
    pub name: String,
    pub symbol: Option<String>,
    /// Items in the collection.
    pub total_supply: Option<u64>,
    pub holders: Option<u64>,
    /// Cheapest active listing, in `floor_currency`.
    pub floor: Option<f64>,
    /// Currency of `floor` (zero address = native QUAI).
    pub floor_currency: String,
    /// Everything ever traded through the marketplace, in QUAI.
    pub volume_quai: Option<f64>,
    pub trades: Option<u64>,
    pub active_listings: Option<u64>,
    pub last_sale: Option<f64>,
    /// Unix seconds of the last sale.
    pub last_sale_at: Option<u64>,
    pub website: Option<String>,
    pub twitter: Option<String>,
}

impl CollectionStats {
    /// Whether `floor` is in native QUAI, the only currency the floors here can be compared in.
    pub fn floor_is_native(&self) -> bool {
        self.floor_currency.is_empty() || is_zero_address(&self.floor_currency)
    }
}

/// One filled sale. `price_quai` is the price in QUAI when the sale was paid in QUAI.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Trade {
    pub tx_hash: String,
    pub contract: String,
    pub token_id: String,
    pub seller: String,
    pub buyer: String,
    /// Price in `currency`'s units, as the indexer reports it.
    pub price_quai: Option<f64>,
    /// Currency contract (zero address = native QUAI).
    pub currency: String,
    /// `ask_filled`, `offer_accepted`, `seaport_filled` or `auction_settled`.
    pub kind: String,
    /// Unix seconds.
    pub at: u64,
    pub name: Option<String>,
    pub image: Option<String>,
}

impl Trade {
    /// Paid in native QUAI, so its price can be summed and compared with others.
    pub fn is_native(&self) -> bool {
        self.currency.is_empty() || is_zero_address(&self.currency)
    }

    /// How the sale happened, for a list.
    pub fn kind_label(&self) -> &str {
        match self.kind.as_str() {
            "ask_filled" => "listing",
            "offer_accepted" => "offer",
            "auction_settled" => "auction",
            "seaport_filled" => "seaport",
            other => other,
        }
    }
}

/// Volume in QUAI and number of sales over the last `days`, counting sales paid in QUAI. Sales in
/// other currencies are counted but not added to the volume, so a total is never a mixed sum.
pub fn trade_window(trades: &[Trade], days: u64, now: u64) -> (f64, usize) {
    let since = now.saturating_sub(days * 86_400);
    let recent = trades.iter().filter(|t| t.at >= since);
    let mut volume = 0.0;
    let mut count = 0;
    for t in recent {
        count += 1;
        if t.is_native() {
            volume += t.price_quai.unwrap_or(0.0);
        }
    }
    (volume, count)
}

/// Parse `/collections` from the Bazarr indexer.
pub fn parse_collection_stats(v: &Value) -> Vec<CollectionStats> {
    let text = |x: &Value| x.as_str().map(clean_text).unwrap_or_default();
    let number = |x: &Value| x.as_f64().or_else(|| x.as_str().and_then(|s| s.parse().ok())).filter(|n: &f64| n.is_finite());
    let count = |x: &Value| x.as_u64().or_else(|| x.as_str().and_then(|s| s.parse().ok()));
    let link = |x: &Value| x.as_str().map(clean_text).filter(|u| u.starts_with("https://")).map(|u| u.chars().take(120).collect());
    v["collections"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|c| {
                    let address = text(&c["address"]).to_lowercase();
                    if !address.starts_with("0x") || address.len() != 42 {
                        return None;
                    }
                    Some(CollectionStats {
                        address,
                        name: text(&c["name"]).chars().take(80).collect(),
                        symbol: c["symbol"].as_str().map(clean_text).filter(|s| !s.is_empty()).map(|s| s.chars().take(16).collect()),
                        total_supply: count(&c["total_supply"]),
                        holders: count(&c["holders_count"]),
                        floor: number(&c["floor_price"]).filter(|f| *f > 0.0),
                        floor_currency: text(&c["floor_currency"]).to_lowercase(),
                        volume_quai: number(&c["total_volume"]),
                        trades: count(&c["total_trades"]),
                        active_listings: count(&c["active_listings"]),
                        last_sale: number(&c["last_sale_price"]),
                        last_sale_at: c["last_sale_at"].as_str().and_then(crate::explorer::parse_timestamp),
                        website: link(&c["website"]),
                        twitter: link(&c["twitter"]),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse `/trades` from the Bazarr indexer, newest first.
pub fn parse_trades(v: &Value) -> Vec<Trade> {
    let text = |x: &Value| x.as_str().map(clean_text).unwrap_or_default();
    let mut rows: Vec<Trade> = v["trades"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|t| {
                    let contract = text(&t["contract"]).to_lowercase();
                    let token_id = text(&t["token_id"]);
                    if !contract.starts_with("0x") || !token_id.chars().all(|c| c.is_ascii_digit()) || token_id.is_empty() {
                        return None;
                    }
                    Some(Trade {
                        tx_hash: text(&t["tx_hash"]),
                        contract,
                        token_id,
                        seller: text(&t["seller"]).to_lowercase(),
                        buyer: text(&t["buyer"]).to_lowercase(),
                        price_quai: t["price_quai"].as_f64().or_else(|| t["price_quai"].as_str().and_then(|s| s.parse().ok())),
                        currency: text(&t["currency"]).to_lowercase(),
                        kind: text(&t["trade_type"]),
                        at: t["timestamp"].as_str().and_then(crate::explorer::parse_timestamp).unwrap_or(0),
                        name: t["meta_name"].as_str().map(clean_text).filter(|n| !n.is_empty()).map(|n| n.chars().take(80).collect()),
                        image: t["meta_image_url"]
                            .as_str()
                            .filter(|u| u.starts_with("https://") || u.starts_with("ipfs://"))
                            .map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    rows.sort_by(|a, b| b.at.cmp(&a.at));
    rows
}

/// Every collection the marketplace indexer knows, with its floor, volume and listing counts.
pub async fn collection_stats(ctx: &DataCtx) -> Result<Vec<CollectionStats>> {
    let base = indexer(ctx)?;
    let url = format!("{}/collections", base.trim_end_matches('/'));
    let cached = ctx.cached("nft_collection_stats", 300, || async { Ok(parse_collection_stats(&http::get_json(&url).await?)) }).await?;
    Ok(cached.value)
}

/// Filled sales, newest first, for one collection or the whole marketplace.
///
/// The indexer has no date filter, so this asks for the newest `MAX_TRADES` and every window is
/// counted here. The whole history is small (a few hundred sales), so one read covers it.
pub async fn trades(ctx: &DataCtx, collection: Option<&str>) -> Result<Vec<Trade>> {
    let base = indexer(ctx)?;
    let url = match collection {
        Some(c) if c.starts_with("0x") && c.len() == 42 && c[2..].chars().all(|ch| ch.is_ascii_hexdigit()) => {
            format!("{}/trades?limit={MAX_TRADES}&contract={}", base.trim_end_matches('/'), c.to_lowercase())
        }
        Some(_) => return Err(CoreError::Invalid("collection must be a contract address".into())),
        None => format!("{}/trades?limit={MAX_TRADES}", base.trim_end_matches('/')),
    };
    let key = format!("nft_trades:{}", collection.unwrap_or("all").to_lowercase());
    let cached = ctx.cached(&key, 120, || async { Ok(parse_trades(&http::get_json(&url).await?)) }).await?;
    Ok(cached.value)
}

/// How many sales one read asks for.
pub const MAX_TRADES: usize = 1000;

/// The marketplace indexer for this network, or why there is none.
fn indexer(ctx: &DataCtx) -> Result<String> {
    if !ctx.policy.market {
        return Err(CoreError::Rejected("market data is turned off (System › Data sources)".into()));
    }
    ctx.network
        .ecosystem
        .bazarr_indexer
        .clone()
        .ok_or_else(|| CoreError::NotFound(format!("no NFT marketplace indexer on {}", ctx.network.name)))
}

/// Active listings, cheapest first (optionally one collection).
pub async fn listings(ctx: &DataCtx, collection: Option<&str>) -> Result<Vec<Listing>> {
    let base = ctx
        .network
        .ecosystem
        .bazarr_indexer
        .clone()
        .ok_or_else(|| CoreError::NotFound(format!("no NFT marketplace indexer on {}", ctx.network.name)))?;
    if !ctx.policy.market {
        return Err(CoreError::Rejected("market data is turned off (System › Data sources)".into()));
    }
    let key = format!("listings:{}", collection.unwrap_or("all").to_lowercase());
    let url = match collection {
        Some(c) if c.starts_with("0x") && c.len() == 42 && c[2..].chars().all(|ch| ch.is_ascii_hexdigit()) => {
            format!("{}/listings?contract={}", base.trim_end_matches('/'), c.to_lowercase())
        }
        Some(_) => return Err(CoreError::Invalid("collection must be a contract address".into())),
        None => format!("{}/listings", base.trim_end_matches('/')),
    };
    let cached = ctx.cached(&key, 60, || async { Ok(parse_listings(&http::get_json(&url).await?)) }).await?;
    let mut rows = cached.value;
    sort_listings(&mut rows, ListingSort::Cheapest);
    Ok(rows)
}

/// Listing order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ListingSort {
    /// Lowest price first (QUAI-priced listings before token-priced ones).
    #[default]
    Cheapest,
    /// Highest price first (QUAI-priced listings before token-priced ones).
    Priciest,
    /// Most recently listed first.
    Newest,
}

impl ListingSort {
    /// Label for headers.
    pub fn label(self) -> &'static str {
        match self {
            ListingSort::Cheapest => "cheapest first",
            ListingSort::Priciest => "priciest first",
            ListingSort::Newest => "newest first",
        }
    }
    /// The next order (for a toggle key).
    pub fn next(self) -> Self {
        match self {
            ListingSort::Cheapest => ListingSort::Priciest,
            ListingSort::Priciest => ListingSort::Newest,
            ListingSort::Newest => ListingSort::Cheapest,
        }
    }
}

/// Sort listings. Prices in different currencies are never compared: QUAI-priced listings come
/// first in both price orders.
pub fn sort_listings(rows: &mut [Listing], sort: ListingSort) {
    rows.sort_by(|a, b| {
        let tie = || a.contract.cmp(&b.contract).then(a.token_id.cmp(&b.token_id));
        match sort {
            ListingSort::Cheapest => {
                b.is_native().cmp(&a.is_native()).then(a.currency.cmp(&b.currency)).then(a.price_amount().cmp(&b.price_amount()))
            }
            ListingSort::Priciest => {
                b.is_native().cmp(&a.is_native()).then(a.currency.cmp(&b.currency)).then(b.price_amount().cmp(&a.price_amount()))
            }
            ListingSort::Newest => b.created_at.cmp(&a.created_at),
        }
        .then_with(tie)
    });
}

/// Collections present in a listing set, most listings first: (contract, count).
pub fn listing_collections(rows: &[Listing]) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for l in rows {
        match counts.iter_mut().find(|(c, _)| *c == l.contract) {
            Some((_, n)) => *n += 1,
            None => counts.push((l.contract.clone(), 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    counts
}

/// Bazarr web page for an item.
pub fn bazarr_url(network: &NetworkProfile, contract: &str, token_id: &str) -> Option<String> {
    network.ecosystem.bazarr_web.as_ref().map(|w| format!("{}/nft/{}-{token_id}", w.trim_end_matches('/'), contract.to_lowercase()))
}

/// A Zora ask as stored on-chain.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ask {
    /// Seller.
    pub seller: String,
    /// Proceeds recipient.
    pub funds_recipient: String,
    /// Currency (zero = native).
    pub currency: String,
    /// Finder's fee.
    pub finders_fee_bps: u16,
    /// Price.
    pub price: String,
}

/// Where a seller stands with the marketplace for one item (read on-chain).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SellerState {
    /// Current owner.
    pub owner: String,
    /// The seller owns the item.
    pub owns: bool,
    /// The Asks module is approved in the module manager.
    pub module_approved: bool,
    /// The ERC-721 transfer helper may move the collection's items.
    pub helper_approved: bool,
    /// The seller's active ask on this item.
    pub ask: Option<Ask>,
}

/// Active listings by the given sellers, from the Bazarr indexer (untrusted until re-checked).
pub async fn listings_by(ctx: &DataCtx, sellers: &[String]) -> Result<Vec<Listing>> {
    let base = ctx
        .network
        .ecosystem
        .bazarr_indexer
        .clone()
        .ok_or_else(|| CoreError::NotFound(format!("no NFT marketplace indexer on {}", ctx.network.name)))?;
    if !ctx.policy.explorer {
        return Err(CoreError::Rejected("address lookups are turned off (System › Data sources)".into()));
    }
    let mut out = Vec::new();
    for seller in sellers {
        let seller = seller.to_lowercase();
        if !(seller.starts_with("0x") && seller.len() == 42 && seller[2..].chars().all(|c| c.is_ascii_hexdigit())) {
            continue;
        }
        let url = format!("{}/listings?seller={seller}", base.trim_end_matches('/'));
        let rows = ctx.cached(&format!("listings_by:{seller}"), 60, || async { Ok(parse_listings(&http::get_json(&url).await?)) }).await?;
        out.extend(rows.value.into_iter().filter(|l| l.seller == seller));
    }
    sort_listings(&mut out, ListingSort::Newest);
    Ok(out)
}

/// A listing currency by name: QUAI (native) or a token the network knows (WQI, WQUAI, USDT).
pub fn listing_currency(network: &NetworkProfile, name: &str) -> Result<(String, String, u8)> {
    let upper = name.trim().to_ascii_uppercase();
    if upper.is_empty() || upper == "QUAI" {
        return Ok((ZERO_ADDRESS.to_string(), "QUAI".into(), QUAI_DECIMALS));
    }
    let candidates = [network.wqi.clone(), network.wquai.clone(), network.ecosystem.usdt.as_ref().map(|u| u.address.clone())];
    for address in candidates.into_iter().flatten() {
        if let Some((symbol, decimals)) = known_currency(network, &address)
            && symbol == upper
        {
            return Ok((address.to_lowercase(), symbol.to_string(), decimals));
        }
    }
    Err(CoreError::Invalid(format!("list in QUAI, WQI, WQUAI or USDT (not `{name}`)")))
}

/// On-chain re-check of a listing.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AskCheck {
    /// The ask read from the module (None: no active ask).
    pub ask: Option<Ask>,
    /// Current ERC-721 owner.
    pub owner: Option<String>,
    /// The seller still owns the item.
    pub seller_owns: bool,
    /// The seller approved the Asks module in the module manager.
    pub seller_module_approved: bool,
    /// The seller approved the ERC-721 transfer helper.
    pub seller_helper_approved: bool,
    /// Buyer must approve the Asks module first (ERC-20 priced asks only).
    pub buyer_module_approval_needed: bool,
    /// Buyer must approve the ERC-20 helper for the price (ERC-20 priced asks only).
    pub buyer_token_approval_needed: bool,
    /// Everything checks out.
    pub valid: bool,
    /// Why not, in plain words.
    pub problems: Vec<String>,
}

fn check_token_id(token_id: &str) -> Result<()> {
    if token_id.is_empty() || token_id.len() > 78 || !token_id.chars().all(|c| c.is_ascii_digit()) {
        return Err(CoreError::Invalid("token id must be a decimal integer".into()));
    }
    Ok(())
}

/// Pinned Zora contracts for a network, verified.
pub struct Zora {
    /// Asks module.
    pub asks: QuaiAddress,
    /// Module manager.
    pub manager: QuaiAddress,
    /// ERC-721 transfer helper.
    pub erc721_helper: QuaiAddress,
    /// ERC-20 transfer helper.
    pub erc20_helper: QuaiAddress,
}

impl Zora {
    /// Verify pinned bytecode and that the module's helpers match the pinned helpers.
    ///
    /// Always first-hand: every caller is either a review (buy, approve, list, cancel) or an
    /// on-chain re-check that a review is about to assert, and all four addresses end up as call
    /// destinations or approval spenders in one.
    pub async fn open(app: &AppDb, node: &Node, network: &NetworkProfile) -> Result<Zora> {
        let eco = &network.ecosystem;
        let missing = || CoreError::Network(format!("NFT buying is not available on {}", network.name));
        // Four code reads that do not depend on each other, and none of them may come from a memo:
        // one after another they were most of what a purchase review spent before drawing.
        let pins = [
            (eco.zora_asks.as_ref().ok_or_else(missing)?, "Zora Asks module"),
            (eco.zora_module_manager.as_ref().ok_or_else(missing)?, "Zora module manager"),
            (eco.zora_erc721_helper.as_ref().ok_or_else(missing)?, "Zora ERC-721 helper"),
            (eco.zora_erc20_helper.as_ref().ok_or_else(missing)?, "Zora ERC-20 helper"),
        ];
        let verified = crate::data::verify_pinned_all(app, node, network, &pins, crate::data::Trust::FirstHand).await?;
        let (asks, manager, erc721_helper, erc20_helper) = (verified[0], verified[1], verified[2], verified[3]);
        let c = Contract::new(asks, interface(ASKS_ABI)?, &node.provider);
        let caller = addr(READ_CALLER)?;
        let (h721, h20) = futures::future::join(
            c.call(caller, "erc721TransferHelper", &[], BlockTag::Latest),
            c.call(caller, "erc20TransferHelper", &[], BlockTag::Latest),
        )
        .await;
        let (h721, h20) = (h721?, h20?);
        let same = |v: &[Value], a: QuaiAddress| v.first().and_then(Value::as_str).is_some_and(|x| x.eq_ignore_ascii_case(&a.to_string()));
        if !same(&h721, erc721_helper) || !same(&h20, erc20_helper) {
            return Err(CoreError::Rejected("the Asks module's transfer helpers do not match the pinned helpers".into()));
        }
        Ok(Zora { asks, manager, erc721_helper, erc20_helper })
    }

    /// Ownership, approvals and the seller's own ask for one item.
    pub async fn seller_state(&self, node: &Node, contract: &str, token_id: &str, seller: &str) -> Result<SellerState> {
        check_token_id(token_id)?;
        let caller = addr(READ_CALLER)?;
        let nft = Contract::new(addr(contract)?, interface(NFT_ABI)?, &node.provider);
        let owner = nft
            .call(caller, "ownerOf", &[json!(token_id)], BlockTag::Latest)
            .await
            .map_err(|_| CoreError::Invalid("could not read the owner; only ERC-721 items can be listed on Zora asks".into()))?
            .first()
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_lowercase();
        let approved = |v: Vec<Value>| v.first().and_then(Value::as_bool).unwrap_or(false);
        let manager = Contract::new(self.manager, interface(MODULE_MANAGER_ABI)?, &node.provider);
        let module_approved =
            approved(manager.call(caller, "isModuleApproved", &[json!(seller), json!(self.asks.to_string())], BlockTag::Latest).await?);
        let helper_approved = approved(
            nft.call(caller, "isApprovedForAll", &[json!(seller), json!(self.erc721_helper.to_string())], BlockTag::Latest).await?,
        );
        let asks = Contract::new(self.asks, interface(ASKS_ABI)?, &node.provider);
        let raw = asks.call(caller, "askForNFT", &[json!(contract), json!(token_id)], BlockTag::Latest).await?;
        let get = |i: usize| raw.get(i).and_then(Value::as_str).unwrap_or_default().to_lowercase();
        let ask_seller = get(0);
        let ask = (!ask_seller.is_empty() && !is_zero_address(&ask_seller) && ask_seller.eq_ignore_ascii_case(seller)).then(|| Ask {
            seller: ask_seller.clone(),
            funds_recipient: get(1),
            currency: get(2),
            finders_fee_bps: get(3).parse().unwrap_or(0),
            price: get(4),
        });
        Ok(SellerState { owns: owner.eq_ignore_ascii_case(seller), owner, module_approved, helper_approved, ask })
    }

    /// Read and validate an ask for `buyer`.
    pub async fn check(&self, node: &Node, contract: &str, token_id: &str, buyer: Option<&str>) -> Result<AskCheck> {
        check_token_id(token_id)?;
        let caller = addr(READ_CALLER)?;
        let asks = Contract::new(self.asks, interface(ASKS_ABI)?, &node.provider);
        let raw = asks.call(caller, "askForNFT", &[json!(contract), json!(token_id)], BlockTag::Latest).await?;
        let get = |i: usize| raw.get(i).and_then(Value::as_str).unwrap_or_default().to_lowercase();
        let mut check = AskCheck {
            ask: None,
            owner: None,
            seller_owns: false,
            seller_module_approved: false,
            seller_helper_approved: false,
            buyer_module_approval_needed: false,
            buyer_token_approval_needed: false,
            valid: false,
            problems: Vec::new(),
        };
        let seller = get(0);
        if seller.is_empty() || is_zero_address(&seller) {
            check.problems.push("this item has no active Zora ask (sold or cancelled)".into());
            return Ok(check);
        }
        let ask = Ask {
            seller: seller.clone(),
            funds_recipient: get(1),
            currency: get(2),
            finders_fee_bps: get(3).parse().unwrap_or(0),
            price: get(4),
        };
        let nft = Contract::new(addr(contract)?, interface(NFT_ABI)?, &node.provider);
        match nft.call(caller, "ownerOf", &[json!(token_id)], BlockTag::Latest).await {
            Ok(v) => {
                let owner = v.first().and_then(Value::as_str).unwrap_or_default().to_lowercase();
                check.seller_owns = owner == seller;
                check.owner = Some(owner);
            }
            Err(_) => check.problems.push("could not read the item's owner (not an ERC-721?)".into()),
        }
        if check.owner.is_some() && !check.seller_owns {
            check.problems.push("the seller no longer owns this item".into());
        }
        let manager = Contract::new(self.manager, interface(MODULE_MANAGER_ABI)?, &node.provider);
        let approved = |v: Vec<Value>| v.first().and_then(Value::as_bool).unwrap_or(false);
        check.seller_module_approved =
            approved(manager.call(caller, "isModuleApproved", &[json!(seller), json!(self.asks.to_string())], BlockTag::Latest).await?);
        if !check.seller_module_approved {
            check.problems.push("the seller revoked the marketplace module".into());
        }
        check.seller_helper_approved = approved(
            nft.call(caller, "isApprovedForAll", &[json!(seller), json!(self.erc721_helper.to_string())], BlockTag::Latest).await?,
        );
        if !check.seller_helper_approved {
            check.problems.push("the seller revoked the marketplace's transfer approval".into());
        }
        if let Some(buyer) = buyer {
            if buyer.eq_ignore_ascii_case(&seller) {
                check.problems.push("you are the seller".into());
            }
            if !is_zero_address(&ask.currency) {
                check.buyer_module_approval_needed = !approved(
                    manager.call(caller, "isModuleApproved", &[json!(buyer), json!(self.asks.to_string())], BlockTag::Latest).await?,
                );
                let erc = Erc20::new(addr(&ask.currency)?, &node.provider)?;
                let allowance = erc.allowance(caller, addr(buyer)?, self.erc20_helper, BlockTag::Latest).await?;
                check.buyer_token_approval_needed = allowance < U256::from_str_radix(&ask.price, 10).unwrap_or(U256::MAX);
            }
        }
        check.valid = check.problems.is_empty();
        check.ask = Some(ask);
        Ok(check)
    }
}

/// An NFT held by the wallet.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct OwnedNft {
    /// Metadata (indexer).
    pub item: NftItem,
    /// Account holding it.
    pub owner: String,
    /// Standard.
    pub kind: TokenKind,
    /// Quantity held (ERC-1155), decimal string.
    pub quantity: String,
    /// Ownership re-read on-chain.
    pub verified: bool,
}

/// How long the indexer's candidate list is reused before replaying transfers again.
const NFT_CANDIDATES_TTL: u64 = 300;

/// NFTs this wallet bought, or sent to `owner`, according to the local operation log. Covers the
/// window before the indexer sees a purchase (and a candidate list cached before it happened).
/// Suggestions only: callers verify ownership on-chain.
pub fn local_nft_candidates(app: &AppDb, network: &str, owner: &str) -> Vec<(String, String, TokenKind, String)> {
    use crate::appdb::OpStatus;
    let Ok(ops) = app.operations(network, 2_000) else { return Vec::new() };
    ops.into_iter()
        .filter(|o| matches!(o.status, OpStatus::Confirmed | OpStatus::Settled))
        .filter(|o| match o.kind.as_str() {
            "nft_buy" => o.account.eq_ignore_ascii_case(owner),
            "nft_transfer" => o.counterparty.eq_ignore_ascii_case(owner),
            _ => false,
        })
        .filter_map(|o| {
            let contract = o.detail.contract().as_str()?.to_lowercase();
            let token_id = o.detail.token_id().as_str()?.to_string();
            let kind =
                if o.detail.standard().as_str() == Some(TokenKind::Erc1155.label()) { TokenKind::Erc1155 } else { TokenKind::Erc721 };
            let quantity = o.detail.quantity().as_str().unwrap_or("1").to_string();
            Some((contract, token_id, kind, quantity))
        })
        .collect()
}

/// Blocks re-read below the newest known transfer, so transfers the indexer adds late are seen.
const TRANSFER_OVERLAP_BLOCKS: u64 = 50;

/// NFT candidates from the address's transfer history, kept in the cache and extended with only
/// the newest pages (explorer.qu.ai returns ids only through transfers). A full replay happens
/// once per address; later refreshes usually cost one request.
async fn nft_candidates_incremental(ctx: &DataCtx, owner: &str, ttl: u64) -> Result<Vec<(String, String, TokenKind, String)>> {
    use crate::explorer::TokenTransfer;
    let key = format!("{}:nft_transfers:{}", ctx.network.id, owner.to_lowercase());
    let held = ctx.app.cache_get(&key)?.and_then(|(text, at)| serde_json::from_str::<Vec<TokenTransfer>>(&text).ok().map(|v| (v, at)));
    let replay = |transfers: &[TokenTransfer]| {
        crate::explorer::replay_nft_holdings(owner, transfers)
            .into_iter()
            .map(|(c, id, k, q)| (c, id, k, q.to_string()))
            .collect::<Vec<_>>()
    };
    if let Some((transfers, at)) = &held
        && crate::registry::now().saturating_sub(*at) < ttl
    {
        return Ok(replay(transfers));
    }
    let mut transfers = held.map(|(v, _)| v).unwrap_or_default();
    let overlap = transfers.iter().map(|t| t.block).max().map(|b| b.saturating_sub(TRANSFER_OVERLAP_BLOCKS));
    let fresh = match ctx.explorer.token_transfers_since(owner, overlap, 10_000).await {
        Ok(f) => f,
        // Serve what is held when the explorer is busy; the next refresh catches up.
        Err(e) if !transfers.is_empty() => {
            let _ = e;
            return Ok(replay(&transfers));
        }
        Err(e) => return Err(e),
    };
    let seen: std::collections::HashSet<(String, u64)> = transfers.iter().map(|t| (t.tx_hash.clone(), t.log_index)).collect();
    transfers.extend(
        fresh
            .into_iter()
            .filter(|t| t.kind != TokenKind::Erc20 && t.token_id.is_some() && !seen.contains(&(t.tx_hash.clone(), t.log_index))),
    );
    if let Ok(text) = serde_json::to_string(&transfers) {
        let _ = ctx.app.cache_put(&key, &text);
    }
    Ok(replay(&transfers))
}

/// NFTs held by the given accounts, ownership verified on-chain (at most `limit` items).
/// `refresh` skips the cached indexer candidates (a user-requested reload).
pub async fn holdings(ctx: &DataCtx, owners: &[String], limit: usize, refresh: bool) -> Result<Vec<OwnedNft>> {
    if !ctx.policy.explorer {
        return Err(CoreError::Rejected("explorer lookups are turned off (System › Data sources)".into()));
    }
    // The last verified result, for a cache-only pass.
    let mut owners_key: Vec<String> = owners.iter().map(|o| o.to_lowercase()).collect();
    owners_key.sort();
    let verified_key = format!("owned_nfts:{}", owners_key.join(","));
    if ctx.cache_only {
        return Ok(ctx.cached(&verified_key, 0, || async { Ok(Vec::<OwnedNft>::new()) }).await?.value);
    }
    let mut out = Vec::new();
    for owner in owners {
        let explorer = &ctx.explorer;
        let ttl = if refresh { 0 } else { NFT_CANDIDATES_TTL };
        // One request with ids and metadata; the transfer replay remains the fallback.
        let listed = ctx
            .cached(&format!("owned_nfts_v2:{}", owner.to_lowercase()), ttl, || explorer.owned_nfts(owner))
            .await
            .ok()
            .map(|c| c.value)
            .filter(|_| explorer.backend == crate::explorer::Backend::Quai);
        let mut metadata: std::collections::HashMap<(String, String), NftItem> = std::collections::HashMap::new();
        let mut candidates = match listed {
            Some(items) => items
                .into_iter()
                .map(|i| {
                    let candidate =
                        (i.contract.to_lowercase(), i.token_id.clone(), i.kind.unwrap_or(TokenKind::Erc721), i.quantity.clone());
                    metadata.insert((candidate.0.clone(), candidate.1.clone()), i);
                    candidate
                })
                .collect(),
            None if explorer.backend == crate::explorer::Backend::Quai => nft_candidates_incremental(ctx, owner, ttl).await?,
            None => {
                ctx.cached(&format!("nft_candidates:{}", owner.to_lowercase()), ttl, || async {
                    Ok(explorer.nft_candidates(owner).await?.into_iter().map(|(c, id, k, q)| (c, id, k, q.to_string())).collect::<Vec<_>>())
                })
                .await?
                .value
            }
        };
        for local in local_nft_candidates(&ctx.app, &ctx.network.id, owner) {
            if !candidates.iter().any(|c| c.0.eq_ignore_ascii_case(&local.0) && c.1 == local.1) {
                candidates.push(local);
            }
        }
        for (contract, token_id, kind, quantity) in candidates {
            if out.len() >= limit {
                break;
            }
            let (verified, quantity) = match kind {
                TokenKind::Erc721 => match ctx.erc721_owner(&contract, &token_id, READ_CALLER).await {
                    Ok(o) => (o.eq_ignore_ascii_case(owner), quantity),
                    Err(_) => (false, quantity),
                },
                _ => match ctx.erc1155_balance(&contract, &token_id, owner).await {
                    Ok(b) => (!b.is_zero(), b.to_string()),
                    Err(_) => (false, quantity),
                },
            };
            if !verified {
                continue;
            }
            let listed = metadata.remove(&(contract.to_lowercase(), token_id.clone())).filter(|i| i.image.is_some() && !i.name.is_empty());
            let fetched = match listed {
                Some(i) => Ok(i),
                None => {
                    ctx.cached(&format!("nft:{contract}:{token_id}"), 86_400, || explorer.nft(&contract, &token_id)).await.map(|c| c.value)
                }
            };
            let item = match fetched {
                // Indexer metadata can predate a sale; the on-chain check above is the owner of record.
                Ok(i) => NftItem { owner: Some(owner.to_lowercase()), ..i },
                Err(_) => NftItem {
                    contract: contract.clone(),
                    token_id: token_id.clone(),
                    name: format!("#{token_id}"),
                    quantity: quantity.clone(),
                    ..NftItem::default()
                },
            };
            // The explorer can fail to read a token's metadata and never retry; the contract still
            // says where it is.
            let item = ctx.with_own_metadata(item, kind).await;
            out.push(OwnedNft { item, owner: owner.to_lowercase(), kind, quantity, verified });
        }
    }
    let _ = ctx.cached(&verified_key, 0, || async { Ok(out.clone()) }).await;
    Ok(out)
}

impl Session {
    /// Symbol and decimals of an ask's payment token, read on-chain (untrusted display data).
    async fn payment_token(&self, currency: &str) -> (String, u8) {
        let Ok(token) = addr(currency) else { return ("TOKEN".into(), 18) };
        let Ok(caller) = addr(READ_CALLER) else { return ("TOKEN".into(), 18) };
        let Ok(erc) = Erc20::new(token, &self.node.provider) else { return ("TOKEN".into(), 18) };
        let symbol = erc
            .contract()
            .call(caller, "symbol", &[], BlockTag::Latest)
            .await
            .ok()
            .and_then(|v| v.first().and_then(Value::as_str).map(crate::ops::sanitize_display))
            .unwrap_or_else(|| "TOKEN".into());
        let decimals = erc
            .contract()
            .call(caller, "decimals", &[], BlockTag::Latest)
            .await
            .ok()
            .and_then(|v| v.first().and_then(Value::as_str).and_then(|d| d.parse::<u8>().ok()))
            .filter(|d| *d <= 77)
            .unwrap_or(18);
        (symbol, decimals)
    }

    fn account_for_owner(&self, owner: Option<&str>) -> Result<crate::registry::QuaiAccount> {
        self.account(owner)
    }

    /// Where this account stands for listing an item.
    pub async fn seller_state(&self, account: Option<&str>, contract: &str, token_id: &str) -> Result<SellerState> {
        let from = self.account(account)?;
        let zora = Zora::open(&self.app, &self.node, &self.network).await?;
        zora.seller_state(&self.node, contract, token_id, &from.address).await
    }

    fn nft_name(&self, contract: &str, token_id: &str) -> Option<String> {
        self.app
            .cache_get(&format!("{}:nft:{}:{token_id}", self.network.id, contract.to_lowercase()))
            .ok()
            .flatten()
            .and_then(|(t, _)| serde_json::from_str::<NftItem>(&t).ok())
            .map(|i| i.name)
            .filter(|n| !n.is_empty())
    }

    /// Review letting the marketplace's transfer helper move this collection's items (once per
    /// collection). Items move only when one of your asks is filled.
    pub async fn review_zora_collection_approval(
        &mut self,
        account: Option<&str>,
        contract: &str,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        let from = self.account(account)?;
        let zora = Zora::open(&self.app, &self.node, &self.network).await?;
        let nft = Contract::new(addr(contract)?, interface(NFT_ABI)?, &self.node.provider);
        let call = nft.prepare("setApprovalForAll", &[json!(zora.erc721_helper.to_string()), json!(true)], U256::ZERO)?;
        let call = with_access_list(&self.node.provider, addr(&from.address)?, call).await?;
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: OpKind::Approve,
            title: "Approve the marketplace for this collection (once)".into(),
            asset: "NFT".into(),
            amount: U256::ZERO,
            decimals: 0,
            counterparty: contract.to_lowercase(),
            fields: vec![
                field("Collection", contract.to_lowercase()),
                field(
                    "Operator",
                    format!(
                        "{} (Zora ERC-721 transfer helper, {})",
                        zora.erc721_helper,
                        self.network.ecosystem.zora_erc721_helper.as_ref().map_or("", |p| p.trust_label_on(&self.node))
                    ),
                ),
                field("Scope", "every item you hold in this collection"),
            ],
            warnings: vec![
                "the helper can move any of your items in this collection, but only the Asks module can ask it to, and only to fill an ask you created".into(),
                "revoke it later with setApprovalForAll(helper, false) if you stop selling; the contracts are unaudited".into(),
            ],
            detail: json!({"purpose": "nft_list", "contract": contract.to_lowercase(), "operator": zora.erc721_helper.to_string()}).into(),
            max_gas: 120_000,
            max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
        })
        .await
    }

    /// Review listing an item for sale (a Zora ask Bazarr shows). Approvals must be in place.
    pub async fn review_nft_list(
        &mut self,
        account: Option<&str>,
        contract: &str,
        token_id: &str,
        price: &str,
        currency: &str,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        self.review_ask(account, contract, token_id, Some((price, currency)), max_fee).await
    }

    /// Review cancelling this account's listing.
    pub async fn review_nft_unlist(
        &mut self,
        account: Option<&str>,
        contract: &str,
        token_id: &str,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        self.review_ask(account, contract, token_id, None, max_fee).await
    }

    /// Create, re-price (`price` with an ask already live) or cancel (`price` None) an ask.
    async fn review_ask(
        &mut self,
        account: Option<&str>,
        contract: &str,
        token_id: &str,
        price: Option<(&str, &str)>,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        let from = self.account(account)?;
        let zora = Zora::open(&self.app, &self.node, &self.network).await?;
        let state = zora.seller_state(&self.node, contract, token_id, &from.address).await?;
        if !state.owns {
            return Err(CoreError::Rejected(format!("{} does not own this item (owner {})", from.label, state.owner)));
        }
        let asks = Contract::new(zora.asks, interface(ASKS_ABI)?, &self.node.provider);
        let name = self.nft_name(contract, token_id);
        let shown = name.clone().unwrap_or_else(|| format!("NFT #{token_id}"));
        let marketplace = format!(
            "{} (Zora Asks v1.1, {}) · shown on Bazarr",
            zora.asks,
            self.network.ecosystem.zora_asks.as_ref().map_or("", |p| p.trust_label_on(&self.node))
        );
        let Some((price_text, currency_name)) = price else {
            let ask = state.ask.ok_or_else(|| CoreError::Rejected("this item has no active listing from this account".into()))?;
            let call = asks.prepare("cancelAsk", &[json!(contract), json!(token_id)], U256::ZERO)?;
            let call = with_access_list(&self.node.provider, addr(&from.address)?, call).await?;
            return self
                .prepare_account(AccountRequest {
                    from,
                    intent: call.into_account_intent(),
                    kind: OpKind::NftUnlist,
                    title: format!("Cancel the listing of {shown}"),
                    asset: "NFT".into(),
                    amount: U256::ZERO,
                    decimals: 0,
                    counterparty: zora.asks.to_string(),
                    fields: vec![field("Collection", contract.to_lowercase()), field("Token id", token_id), field("Marketplace", marketplace)],
                    warnings: vec![],
                    detail: json!({"contract": contract.to_lowercase(), "token_id": token_id, "name": name, "price": ask.price, "currency": ask.currency}).into(),
                    max_gas: 150_000,
                    max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
                })
                .await;
        };
        if !state.module_approved {
            return Err(CoreError::Rejected("approve the marketplace module first (step 1)".into()));
        }
        if !state.helper_approved {
            return Err(CoreError::Rejected("approve the marketplace for this collection first (step 2)".into()));
        }
        let (currency, symbol, decimals) = listing_currency(&self.network, currency_name)?;
        let amount_base = amount::parse_amount(price_text, decimals)?;
        if amount_base.is_zero() {
            return Err(CoreError::Invalid("the price must be above zero".into()));
        }
        let repricing = state.ask.is_some();
        let call = if repricing {
            asks.prepare("setAskPrice", &[json!(contract), json!(token_id), json!(amount_base.to_string()), json!(currency)], U256::ZERO)?
        } else {
            asks.prepare(
                "createAsk",
                &[json!(contract), json!(token_id), json!(amount_base.to_string()), json!(currency), json!(from.address), json!(0)],
                U256::ZERO,
            )?
        };
        let call = with_access_list(&self.node.provider, addr(&from.address)?, call).await?;
        let price_shown = format!("{} {symbol}", amount::format_amount(amount_base, decimals));
        let mut fields = vec![
            field("Collection", contract.to_lowercase()),
            field("Token id", token_id),
            field("Price", price_shown.clone()),
            field("Proceeds to", from.address.clone()),
            field("Royalties", "paid from the price by the contract when it sells"),
            field("Marketplace", marketplace),
        ];
        if let Some(old) = &state.ask {
            let (old_symbol, old_decimals) = known_currency(&self.network, &old.currency).unwrap_or(("QUAI", QUAI_DECIMALS));
            fields.insert(
                2,
                field(
                    "Current price",
                    format!(
                        "{} {old_symbol}",
                        amount::format_amount(U256::from_str_radix(&old.price, 10).unwrap_or_default(), old_decimals)
                    ),
                ),
            );
        }
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: if repricing { OpKind::NftReprice } else { OpKind::NftList },
            title: if repricing { format!("Change the price of {shown}") } else { format!("List {shown} for sale") },
            asset: symbol.clone(),
            amount: amount_base,
            decimals,
            counterparty: zora.asks.to_string(),
            fields,
            warnings: vec![
                format!("anyone can buy it for {price_shown} until you cancel; the sale settles on-chain without asking you again"),
                "Zora V3 fork contracts are unaudited".into(),
            ],
            detail: json!({"contract": contract.to_lowercase(), "token_id": token_id, "name": name, "price": amount_base.to_string(), "currency": currency, "symbol": symbol, "decimals": decimals}).into(),
            max_gas: 250_000,
            max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
        })
        .await
    }

    /// Watch this wallet's listings: when an ask disappears and the item left the account, record
    /// the sale and notify. Throttled; indexer-free (reads the chain).
    pub async fn watch_listings(&mut self) -> Result<Vec<String>> {
        use crate::appdb::OpStatus;
        let key = format!("{}:listing_watch_at", self.network.id);
        let last: u64 = self.app.kv(&key)?.and_then(|v| v.parse().ok()).unwrap_or(0);
        if crate::registry::now().saturating_sub(last) < 30 {
            return Ok(Vec::new());
        }
        self.app.set_kv(&key, &crate::registry::now().to_string())?;
        let ops = self.app.operations(&self.network.id, 2_000)?;
        // Newest listing-related operation per item decides whether it is still being watched.
        let mut latest: Vec<&crate::appdb::Operation> = Vec::new();
        for op in
            ops.iter().filter(|o| matches!(o.kind, OpKind::NftList | OpKind::NftReprice | OpKind::NftUnlist) && o.status == OpStatus::Confirmed)
        {
            let same = |o: &&crate::appdb::Operation| {
                o.detail.contract() == op.detail.contract() && o.detail.token_id() == op.detail.token_id()
            };
            if !latest.iter().any(same) {
                latest.push(op);
            }
        }
        let watched: Vec<crate::appdb::Operation> =
            latest.into_iter().filter(|o| o.kind != OpKind::NftUnlist && o.detail.closed().is_null()).cloned().collect();
        if watched.is_empty() {
            return Ok(Vec::new());
        }
        let zora = Zora::open(&self.app, &self.node, &self.network).await?;
        let mut sold = Vec::new();
        for op in watched {
            let (Some(contract), Some(token_id)) = (op.detail.contract().as_str(), op.detail.token_id().as_str()) else { continue };
            let state = match zora.seller_state(&self.node, contract, token_id, &op.account).await {
                Ok(s) => s,
                Err(_) => continue,
            };
            if state.ask.is_some() {
                continue;
            }
            let name = op.detail.name().as_str().map(str::to_string).unwrap_or_else(|| format!("NFT #{token_id}"));
            let outcome = if state.owns { "ended" } else { "sold" };
            self.app.update_operation(
                &op.id,
                op.status,
                None,
                None,
                Some(&crate::journal::Detail::from(json!({"closed": outcome, "closed_at": crate::registry::now(), "buyer": state.owner}))),
            )?;
            if outcome == "sold" {
                let decimals = op.detail.decimals().as_u64().unwrap_or(18) as u8;
                let symbol = op.detail.symbol().as_str().unwrap_or("QUAI").to_string();
                let price = U256::from_str_radix(op.detail.price().as_str().unwrap_or("0"), 10).unwrap_or_default();
                let body = format!(
                    "{name} sold for {} {symbol} · buyer {}",
                    amount::format_amount(price, decimals),
                    crate::session::short_address(&state.owner)
                );
                self.app.notify("success", "NFT sold", &body)?;
                self.app.record_activity(&crate::appdb::Activity {
                    network: self.network.id.clone(),
                    key: format!("nft_sale:{contract}:{token_id}:{}", op.id),
                    direction: "out".into(),
                    asset: symbol,
                    amount: price.to_string(),
                    address: op.account.clone(),
                    tx_hash: None,
                    block: None,
                    detail: json!({"source": "marketplace", "sale": true, "contract": contract, "token_id": token_id, "name": name, "buyer": state.owner, "decimals": decimals}).into(),
                    observed: crate::registry::now(),
                })?;
                sold.push(body);
            } else {
                self.app.notify("info", "Listing ended", &format!("{name} is no longer listed and is still in your wallet"))?;
            }
        }
        Ok(sold)
    }

    /// Review transferring an NFT (ownership re-checked on-chain).
    pub async fn review_nft_transfer(
        &mut self,
        account: Option<&str>,
        contract: &str,
        token_id: &str,
        to: &str,
        quantity: Option<&str>,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        check_token_id(token_id)?;
        let from = self.account_for_owner(account)?;
        let recipient = match self.resolve_recipient(to)? {
            crate::ops::Recipient::Quai(a) => a,
            _ => return Err(CoreError::Invalid("NFT recipients must be Quai addresses".into())),
        };
        if recipient.to_string().eq_ignore_ascii_case(&from.address) {
            return Err(CoreError::Invalid("that is the same account".into()));
        }
        let collection = addr(contract)?;
        let caller = addr(READ_CALLER)?;
        let nft = Contract::new(collection, interface(NFT_ABI)?, &self.node.provider);
        let owner = nft.call(caller, "ownerOf", &[json!(token_id)], BlockTag::Latest).await;
        let (call, kind, qty) = match owner {
            Ok(v) => {
                let owner = v.first().and_then(Value::as_str).unwrap_or_default();
                if !owner.eq_ignore_ascii_case(&from.address) {
                    return Err(CoreError::Rejected(format!("{} does not own this item on-chain (owner {owner})", from.label)));
                }
                (
                    nft.prepare("safeTransferFrom", &[json!(from.address), json!(recipient.to_string()), json!(token_id)], U256::ZERO)?,
                    TokenKind::Erc721,
                    U256::from(1),
                )
            }
            Err(_) => {
                let multi = Contract::new(collection, interface(ERC1155_ABI)?, &self.node.provider);
                let balance = multi
                    .call(caller, "balanceOf", &[json!(from.address), json!(token_id)], BlockTag::Latest)
                    .await?
                    .first()
                    .and_then(Value::as_str)
                    .and_then(|t| U256::from_str_radix(t, 10).ok())
                    .unwrap_or_default();
                let qty = match quantity {
                    Some(q) => {
                        U256::from_str_radix(q.trim(), 10).map_err(|_| CoreError::Invalid("quantity must be a whole number".into()))?
                    }
                    None => U256::from(1),
                };
                if qty.is_zero() || balance < qty {
                    return Err(CoreError::Insufficient(format!("{} holds {balance} of this item", from.label)));
                }
                (
                    multi.prepare(
                        "safeTransferFrom",
                        &[json!(from.address), json!(recipient.to_string()), json!(token_id), json!(qty.to_string()), json!("0x")],
                        U256::ZERO,
                    )?,
                    TokenKind::Erc1155,
                    qty,
                )
            }
        };
        let name = self
            .app
            .cache_get(&format!("{}:nft:{}:{token_id}", self.network.id, contract.to_lowercase()))?
            .and_then(|(t, _)| serde_json::from_str::<NftItem>(&t).ok())
            .map(|i| i.name);
        let call = with_access_list(&self.node.provider, addr(&from.address)?, call).await?;
        let mut warnings = Vec::new();
        if self.app.contacts()?.iter().all(|c| !c.address.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(&recipient.to_string()))) {
            warnings.push("the recipient is not in your contacts — NFT transfers cannot be undone".into());
        }
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: OpKind::NftTransfer,
            title: format!("Transfer {}", name.clone().unwrap_or_else(|| format!("NFT #{token_id}"))),
            asset: "NFT".into(),
            amount: qty,
            decimals: 0,
            counterparty: recipient.to_string(),
            fields: vec![
                field("Collection", collection.to_string()),
                field("Token id", token_id),
                field("Standard", kind.label()),
                field("Recipient", recipient.to_string()),
            ],
            warnings,
            detail: json!({"contract": contract.to_lowercase(), "token_id": token_id, "standard": kind.label(), "name": name, "quantity": qty.to_string()}).into(),
            max_gas: 250_000,
            max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
        })
        .await
    }

    /// Check a listing for this wallet's account (on-chain).
    pub async fn check_listing(&self, account: Option<&str>, contract: &str, token_id: &str) -> Result<AskCheck> {
        // Watch-only wallets (no account) can still check whether a listing is valid.
        let buyer = match account {
            Some(_) => Some(self.account(account)?.address),
            None => self.account(None).ok().map(|a| a.address),
        };
        let zora = Zora::open(&self.app, &self.node, &self.network).await?;
        zora.check(&self.node, contract, token_id, buyer.as_deref()).await
    }

    /// Review approving the Zora Asks module (once; ERC-20 priced asks only).
    pub async fn review_zora_module_approval(&mut self, account: Option<&str>, max_fee: Option<&str>) -> Result<Review> {
        let from = self.account(account)?;
        let zora = Zora::open(&self.app, &self.node, &self.network).await?;
        let manager = Contract::new(zora.manager, interface(MODULE_MANAGER_ABI)?, &self.node.provider);
        let call = manager.prepare("setApprovalForModule", &[json!(zora.asks.to_string()), json!(true)], U256::ZERO)?;
        let call = with_access_list(&self.node.provider, addr(&from.address)?, call).await?;
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: OpKind::Approve,
            title: "Approve marketplace module (once)".into(),
            asset: "NFT".into(),
            amount: U256::ZERO,
            decimals: 0,
            counterparty: zora.manager.to_string(),
            fields: vec![
                field(
                    "Module manager",
                    format!(
                        "{} (Zora, {})",
                        zora.manager,
                        self.network.ecosystem.zora_module_manager.as_ref().map_or("", |p| p.trust_label_on(&self.node))
                    ),
                ),
                field(
                    "Module",
                    format!(
                        "{} (Asks v1.1, {})",
                        zora.asks,
                        self.network.ecosystem.zora_asks.as_ref().map_or("", |p| p.trust_label_on(&self.node))
                    ),
                ),
            ],
            warnings: vec!["lets the Zora Asks module move tokens you approve to its transfer helpers; the contracts are unaudited".into()],
            detail: json!({"purpose": "nft_buy", "module": zora.asks.to_string()}).into(),
            max_gas: 120_000,
            max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
        })
        .await
    }

    /// Review the exact ERC-20 approval an ERC-20 priced ask needs.
    pub async fn review_zora_token_approval(
        &mut self,
        account: Option<&str>,
        contract: &str,
        token_id: &str,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        let from = self.account(account)?;
        let zora = Zora::open(&self.app, &self.node, &self.network).await?;
        let check = zora.check(&self.node, contract, token_id, Some(&from.address)).await?;
        let ask = check.ask.ok_or_else(|| CoreError::Rejected(check.problems.join("; ")))?;
        if is_zero_address(&ask.currency) {
            return Err(CoreError::Invalid("this ask is priced in QUAI; no token approval is needed".into()));
        }
        let price = U256::from_str_radix(&ask.price, 10).map_err(|_| CoreError::Network("bad ask price".into()))?;
        let call = Erc20::new(addr(&ask.currency)?, &self.node.provider)?.approve(zora.erc20_helper, price)?;
        let call = with_access_list(&self.node.provider, addr(&from.address)?, call).await?;
        let (symbol, decimals) = self.payment_token(&ask.currency).await;
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: OpKind::Approve,
            title: format!("Approve {symbol} for NFT purchase"),
            asset: symbol.clone(),
            amount: price,
            decimals,
            counterparty: zora.erc20_helper.to_string(),
            fields: vec![
                field("Token", ask.currency.clone()),
                field(
                    "Spender",
                    format!(
                        "{} (Zora ERC-20 helper, {})",
                        zora.erc20_helper,
                        self.network.ecosystem.zora_erc20_helper.as_ref().map_or("", |p| p.trust_label_on(&self.node))
                    ),
                ),
                field("Allowance", format!("exactly {} {symbol}", amount::format_amount(price, decimals))),
            ],
            warnings: vec![],
            detail: json!({"token": ask.currency, "purpose": "nft_buy", "decimals": decimals}).into(),
            max_gas: 120_000,
            max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
        })
        .await
    }

    /// Review buying a Zora ask (re-checked on-chain; price must match what the user saw).
    pub async fn review_nft_buy(
        &mut self,
        account: Option<&str>,
        contract: &str,
        token_id: &str,
        expected_price: Option<&str>,
        max_fee: Option<&str>,
    ) -> Result<Review> {
        let from = self.account(account)?;
        let zora = Zora::open(&self.app, &self.node, &self.network).await?;
        let check = zora.check(&self.node, contract, token_id, Some(&from.address)).await?;
        if !check.valid {
            return Err(CoreError::Rejected(format!("cannot buy: {}", check.problems.join("; "))));
        }
        let ask = check.ask.clone().ok_or_else(|| CoreError::Rejected("no active ask".into()))?;
        if check.buyer_module_approval_needed {
            return Err(CoreError::Rejected("approve the marketplace module first (step 1)".into()));
        }
        if check.buyer_token_approval_needed {
            return Err(CoreError::Rejected("approve the payment token first (step 2)".into()));
        }
        let price = U256::from_str_radix(&ask.price, 10).map_err(|_| CoreError::Network("bad ask price".into()))?;
        if let Some(expected) = expected_price
            && U256::from_str_radix(expected, 10).ok() != Some(price)
        {
            return Err(CoreError::Rejected(format!(
                "the price changed since the listing was shown: now {} QUAI",
                amount::format_amount(price, QUAI_DECIMALS)
            )));
        }
        let native = is_zero_address(&ask.currency);
        if native {
            let balance = self.node.provider.balance(addr(&from.address)?, BlockTag::Latest).await?;
            if balance < price {
                return Err(CoreError::Insufficient(format!("QUAI balance is {}", amount::quai(balance))));
            }
        }
        let asks = Contract::new(zora.asks, interface(ASKS_ABI)?, &self.node.provider);
        let call = asks.prepare(
            "fillAsk",
            &[
                json!(contract),
                json!(token_id),
                json!(if native { ZERO_ADDRESS.to_string() } else { ask.currency.clone() }),
                json!(price.to_string()),
                json!(ZERO_ADDRESS),
            ],
            if native { price } else { U256::ZERO },
        )?;
        let call = with_access_list(&self.node.provider, addr(&from.address)?, call).await?;
        let name = self
            .app
            .cache_get(&format!("{}:nft:{}:{token_id}", self.network.id, contract.to_lowercase()))?
            .and_then(|(t, _)| serde_json::from_str::<NftItem>(&t).ok())
            .map(|i| i.name);
        let title = format!("Buy {}", name.clone().unwrap_or_else(|| format!("NFT #{token_id}")));
        let (symbol, decimals) = if native { ("QUAI".to_string(), QUAI_DECIMALS) } else { self.payment_token(&ask.currency).await };
        self.prepare_account(AccountRequest {
            from,
            intent: call.into_account_intent(),
            kind: OpKind::NftBuy,
            title,
            asset: symbol.clone(),
            amount: price,
            decimals,
            counterparty: ask.seller.clone(),
            fields: vec![
                field("Collection", contract.to_lowercase()),
                field("Token id", token_id),
                field("Seller", ask.seller.clone()),
                field("Seller still owns it", "yes (checked on-chain)"),
                field("Marketplace", format!("{} (Zora Asks v1.1, {})", zora.asks, self.network.ecosystem.zora_asks.as_ref().map_or("", |p| p.trust_label_on(&self.node)))),
                field("Price", if native { format!("{} QUAI", amount::format_amount(price, QUAI_DECIMALS)) } else { format!("{} {symbol} ({})", amount::format_amount(price, decimals), ask.currency) }),
                field("Royalties and fees", "paid from the price by the contract"),
            ],
            warnings: vec!["Zora V3 fork contracts are unaudited; the purchase executes exactly as simulated or reverts".into()],
            detail: json!({"contract": contract.to_lowercase(), "token_id": token_id, "name": name, "seller": ask.seller, "currency": ask.currency, "decimals": decimals}).into(),
            max_gas: 600_000,
            max_fee: self.parse_fee_cap(max_fee, QUAI_DECIMALS)?,
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_indexer_listings() {
        let v: Value = serde_json::from_str(include_str!("fixtures/bazarr_listings.json")).unwrap();
        let l = parse_listings(&v);
        assert_eq!(l.len(), 4);
        let zora: Vec<_> = l.iter().filter(|x| x.buyable()).collect();
        assert_eq!(zora.len(), 3);
        assert!(zora[0].is_native() && zora[0].price_text().ends_with("QUAI"));
        let mainnet = NetworkProfile::builtins().remove(0);
        let mut wqi = zora[0].clone();
        wqi.currency = "0x002b2596EcF05C93a31ff916E8b456DF6C77c750".into();
        wqi.price = "10000000000000000000".into();
        assert_eq!(wqi.price_text_on(&mainnet), "10 WQI");
        wqi.currency = "0x0049f7cbca3556c2dfae62aafa7015f99de1b8f5".into();
        wqi.price = "2500000".into();
        assert_eq!(wqi.price_text_on(&mainnet), "2.5 USDT");
        wqi.currency = "0x00ffffffffffffffffffffffffffffffffffffff".into();
        assert!(wqi.price_text_on(&mainnet).contains(" of 0x00ff"));
        let seaport = l.iter().find(|x| x.protocol == "seaport").unwrap();
        assert!(!seaport.buyable());
        // Garbage rows are dropped.
        let bad = json!({"listings": [{"contract": "0x00ab", "token_id": "1; DROP", "price": "1", "seller": "0x1"}, {"contract":"0x00ab","token_id":"2","price":"x","seller":"0x1"}]});
        assert!(parse_listings(&bad).is_empty());
    }

    #[test]
    fn listing_currencies() {
        let mainnet = NetworkProfile::builtins().remove(0);
        assert_eq!(listing_currency(&mainnet, "quai").unwrap(), (ZERO_ADDRESS.to_string(), "QUAI".into(), 18));
        assert_eq!(listing_currency(&mainnet, "").unwrap().1, "QUAI");
        let (usdt, symbol, decimals) = listing_currency(&mainnet, "USDT").unwrap();
        assert_eq!((symbol.as_str(), decimals), ("USDT", 6));
        assert!(usdt.eq_ignore_ascii_case("0x0049F7cbCa3556C2DfaE62Aafa7015F99de1b8f5"));
        assert_eq!(listing_currency(&mainnet, "wqi").unwrap().1, "WQI");
        assert!(listing_currency(&mainnet, "DOGE").is_err());
        let abi = interface(ASKS_ABI).unwrap();
        let _ = abi;
    }

    #[test]
    fn local_candidates_cover_unindexed_purchases() {
        use crate::appdb::{OpStatus, Operation};
        let db = AppDb::memory().unwrap();
        let me = "0x004dd9AFAA2768642B5cDe15c24F37bF19d842E4";
        let op = |id: &str, kind: &str, status: OpStatus, account: &str, counterparty: &str, detail: Value| Operation {
            id: id.into(),
            network: "mainnet".into(),
            kind: crate::journal::OpKind::parse(kind),
            store: "quai".into(),
            account: account.into(),
            status,
            tx_hash: None,
            asset: "NFT".into(),
            amount: "1".into(),
            counterparty: counterparty.into(),
            fee: String::new(),
            detail: detail.into(),
            created: crate::registry::now(),
            updated: crate::registry::now(),
        };
        let bought = json!({"contract": "0x0046E5085a830567F647fE52672926bedc8d5c55", "token_id": "224"});
        db.insert_operation(&op("aa01", "nft_buy", OpStatus::Confirmed, me, "0x00170d", bought.clone())).unwrap();
        db.insert_operation(&op("aa02", "nft_buy", OpStatus::Failed, me, "0x00170d", json!({"contract": "0x0099", "token_id": "1"})))
            .unwrap();
        db.insert_operation(&op(
            "aa03",
            "nft_buy",
            OpStatus::Confirmed,
            "0x0011",
            "0x00170d",
            json!({"contract": "0x0098", "token_id": "2"}),
        ))
        .unwrap();
        let multi = json!({"contract": "0x0077", "token_id": "5", "standard": "ERC-1155", "quantity": "3"});
        db.insert_operation(&op("aa04", "nft_transfer", OpStatus::Confirmed, "0x0011", me, multi)).unwrap();
        let mut got = local_nft_candidates(&db, "mainnet", &me.to_lowercase());
        got.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        assert_eq!(
            got,
            vec![
                ("0x0046e5085a830567f647fe52672926bedc8d5c55".into(), "224".into(), TokenKind::Erc721, "1".into()),
                ("0x0077".into(), "5".into(), TokenKind::Erc1155, "3".into()),
            ]
        );
        assert!(local_nft_candidates(&db, "orchard", me).is_empty());
    }

    #[test]
    fn listing_sort_and_collections() {
        let l = |c: &str, id: &str, price: u64, currency: &str, at: u64| Listing {
            contract: c.into(),
            token_id: id.into(),
            seller: "0x00aa".into(),
            price: price.to_string(),
            currency: currency.into(),
            protocol: "zora".into(),
            quantity: "1".into(),
            created_at: at,
            name: None,
            image: None,
        };
        let zero = "0x0000000000000000000000000000000000000000";
        let token = "0x0049f7cbca3556c2dfae62aafa7015f99de1b8f5";
        let mut rows = vec![
            l("0x00b", "1", 50, zero, 10),
            l("0x00a", "2", 5, zero, 30),
            l("0x00a", "3", 1, token, 40),
            l("0x00a", "4", 900, zero, 20),
        ];
        let ids = |r: &[Listing]| r.iter().map(|x| x.token_id.clone()).collect::<Vec<_>>().join(",");
        sort_listings(&mut rows, ListingSort::Cheapest);
        assert_eq!(ids(&rows), "2,1,4,3", "QUAI prices ascending, then the token-priced listing");
        sort_listings(&mut rows, ListingSort::Priciest);
        assert_eq!(ids(&rows), "4,1,2,3");
        sort_listings(&mut rows, ListingSort::Newest);
        assert_eq!(ids(&rows), "3,2,4,1");
        assert_eq!(listing_collections(&rows), vec![("0x00a".to_string(), 3), ("0x00b".to_string(), 1)]);
        assert_eq!(ListingSort::Newest.next(), ListingSort::Cheapest);
    }

    /// The indexer's collection rows parse into comparable numbers, and a floor priced in a token
    /// is marked as such so it is never compared with a QUAI floor.
    #[test]
    fn collection_stats_parse() {
        let v: Value = serde_json::from_str(include_str!("fixtures/bazarr_collections.json")).unwrap();
        let rows = parse_collection_stats(&v);
        assert_eq!(rows.len(), 3);
        let first = &rows[0];
        assert!(first.address.starts_with("0x") && first.address.len() == 42);
        assert!(first.floor.is_some_and(|f| f > 0.0), "a floor is a positive number");
        assert!(first.floor_is_native(), "priced in QUAI");
        assert!(first.total_supply.is_some() && first.holders.is_some());
        assert!(rows.iter().any(|c| c.trades.is_some_and(|t| t > 0)), "some collection has traded");
        assert!(rows.iter().any(|c| c.floor.is_none()), "a collection with nothing listed has no floor");
    }

    /// Sales parse newest first, and a window counts only what falls inside it. Volume adds up
    /// only the QUAI-paid sales, so a total is never a mixed-currency sum.
    #[test]
    fn trades_parse_and_window() {
        let v: Value = serde_json::from_str(include_str!("fixtures/bazarr_trades.json")).unwrap();
        let rows = parse_trades(&v);
        assert_eq!(rows.len(), 6);
        assert!(rows.windows(2).all(|w| w[0].at >= w[1].at), "newest first");
        assert_eq!(rows[0].kind_label(), "listing");
        let now = rows[0].at;
        let (volume, count) = trade_window(&rows, 3650, now);
        assert_eq!(count, 6, "every sale is inside a ten-year window");
        let native: f64 = rows.iter().filter(|t| t.is_native()).filter_map(|t| t.price_quai).sum();
        assert!((volume - native).abs() < 1e-9, "volume adds the QUAI-paid sales");
        let (_, recent) = trade_window(&rows, 0, now);
        assert!(recent <= 6);
        let (_, none) = trade_window(&rows, 1, now.saturating_add(100 * 86_400));
        assert_eq!(none, 0, "a window after every sale is empty");
    }

    #[test]
    fn bazarr_links_and_abis() {
        let mainnet = NetworkProfile::builtins().remove(0);
        assert_eq!(bazarr_url(&mainnet, "0x00ABC", "7").unwrap(), "https://bazarr.xyz/nft/0x00abc-7");
        interface(ASKS_ABI).unwrap();
        interface(MODULE_MANAGER_ABI).unwrap();
        assert!(check_token_id("12").is_ok() && check_token_id("-1").is_err() && check_token_id("").is_err());
    }
}
