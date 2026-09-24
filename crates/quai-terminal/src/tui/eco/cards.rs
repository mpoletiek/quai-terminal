//! The exchange cards (Swap, Convert, Wrap): quotes, amounts, token pickers and their keys.

use super::*;

impl App {
    pub(crate) fn default_receive_asset(&self) -> Option<SwapAsset> {
        let network = self.net()?;
        let usdt = network.ecosystem.usdt.as_ref().map(|u| u.address.to_lowercase());
        let pick = |address: &str, fallback: &str| {
            let market = self.eco.markets.iter().find(|m| m.address == address);
            SwapAsset::Token {
                address: address.to_string(),
                symbol: market.map(|m| m.symbol.clone()).unwrap_or_else(|| fallback.to_string()),
                decimals: if fallback == "USDT" { 6 } else { 18 },
            }
        };
        usdt.map(|u| pick(&u, "USDT")).or_else(|| network.wqi.as_ref().map(|w| pick(&w.to_lowercase(), "WQI")))
    }

    /// Read what the observed transaction on screen carried and cost, once per hash. Only rows the
    /// wallet did not send need it — its own operations recorded their value and fee.
    pub(crate) fn tick_tx_cost(&mut self) {
        let key = match self.detail.last() {
            Some(Detail::Activity(k)) => Some(k.clone()),
            Some(_) => None,
            None if self.screen == Screen::Activity => match self.activity_rows().get(self.selected) {
                Some((_, false, i)) => self.dash.activity.get(*i).map(|a| format!("act:{}", a.key)),
                _ => None,
            },
            None => None,
        };
        let Some(k) = key.as_deref().and_then(|k| k.strip_prefix("act:")) else { return };
        let Some(hash) = self.dash.activity.iter().find(|a| a.key == k && a.asset != "QI").and_then(|a| a.tx_hash.clone()) else {
            return;
        };
        if self.eco.tx_costs_asked.insert(hash.clone()) {
            self.send_data(DataCmd::TxCost(hash));
        }
    }

    /// Ask for both QUAI ⇄ Qi markets once the amount stops changing.
    pub(crate) fn tick_qi_routes(&mut self) {
        use wallet_core::qi_market::Direction;
        let card = &self.eco.convert;
        let direction = if card.qi_to_quai { Direction::QiToQuai } else { Direction::QuaiToQi };
        let Ok(atoms) = wallet_core::amount::parse_amount(&card.amount, direction.pay_decimals()) else { return };
        if atoms.is_zero() {
            return;
        }
        let slippage = self.swap_slippage();
        let key = hash_key(&[direction.as_str(), &atoms.to_string(), &slippage.to_string()]);
        let debounced = card.edited.is_none_or(|t| t.elapsed() > Duration::from_millis(450));
        // Rates move with the pools and the block's conversion flow.
        let stale = card.routes_key == key
            && card.routes.is_some()
            && self.eco.convert_quoted_at().is_some_and(|t| t.elapsed() > Duration::from_secs(20));
        // The protocol route's own quote is a separate read from the two-market comparison, and it
        // is the one that carries what the discount costs at this instant, the batch scenarios and
        // the suggested tolerance. Without asking for it here the panel beside the card stays empty
        // until the user presses Enter, so the estimate they most need is the one they never see.
        let want_quote = (!card.market).then(|| (card.qi_to_quai, card.amount.clone()));
        let quote_wanted = want_quote.as_ref().is_some_and(|w| card.quoted_for.as_ref() != Some(w)) || stale;
        if debounced && (key != card.requested_key || stale) {
            let owner = self.dash.accounts.first().map(|a| a.address.clone());
            self.eco.convert.requested_key = key;
            self.eco.convert.quoted_at = Some(Instant::now());
            self.send_data(DataCmd::QiRoutes { key, direction, amount: atoms.to_string(), owner, slippage });
        }
        if debounced
            && quote_wanted
            && let Some((qi_to_quai, amount)) = want_quote
        {
            self.eco.convert.quoted_for = Some((qi_to_quai, amount.clone()));
            self.eco.convert.quote = None;
            self.send(Cmd::Quote { direction: direction.as_str().into(), amount });
        }
    }

    /// The slippage the market route quotes with (the swap card's setting).
    pub fn swap_slippage(&self) -> u16 {
        self.eco.swap.slippage_bps
    }

    /// Current means matching normalized intent, owner/network, latest request and age.
    pub fn swap_quote_current(&self) -> bool {
        self.eco.swap.quote_key != 0
            && self.eco.swap.quote_key == self.eco.swap.requested_key
            && self.swap_input_key().is_some_and(|input| Some(input) == self.eco.swap.requested_input)
            && self.eco.swap.quoted_at.is_some_and(|at| at.elapsed() < Duration::from_secs(20))
    }

    /// `t` from anywhere: open Swap with the focused token as the pay side.
    pub fn open_trade(&mut self) {
        let focused = match (self.detail.last(), self.screen) {
            (Some(Detail::Asset(id)), _) => Some(id.clone()),
            (None, Screen::Home) if self.pane == 0 => {
                self.eco.portfolio.as_ref().and_then(|p| p.rows.get(self.selected)).map(|r| r.key.id())
            }
            _ => None,
        };
        if let Some(id) = focused
            && let Some(asset) = self.swap_asset_for(&id)
        {
            if self.eco.swap.to.as_ref() == Some(&asset) {
                self.eco.swap.to = Some(self.eco.swap.from.clone());
            }
            self.eco.swap.from = asset;
            self.eco.swap.quote = None;
        }
        self.switch(Screen::Swap);
        self.eco.swap.field = 1;
    }

    pub(crate) fn swap_asset_for(&self, id: &str) -> Option<SwapAsset> {
        match id {
            "quai" => Some(SwapAsset::Quai),
            "qi" => None,
            address => {
                let row = self.eco.portfolio.as_ref().and_then(|p| p.rows.iter().find(|r| r.key.id() == address));
                Some(SwapAsset::Token {
                    address: address.to_string(),
                    symbol: row.map(|r| r.symbol.clone()).unwrap_or_else(|| wallet_core::session::short_address(address)),
                    decimals: row.map(|r| r.decimals).unwrap_or(18),
                })
            }
        }
    }

    /// Open the swap card on this asset: `buy` pays with the counter asset to get it, otherwise
    /// it sells the asset for the counter asset. The counter is QUAI, or WQI when the asset is
    /// QUAI itself, so both sides are never the same thing.
    pub(crate) fn quick_swap(&mut self, id: &str, buy: bool) -> bool {
        let Some(asset) = self.swap_asset_for(id) else {
            self.toast("Qi is not traded on Quainance — use Convert", true);
            return true;
        };
        let counter = if id == "quai" {
            let wqi = self.net().and_then(|n| n.wqi.clone());
            match wqi {
                Some(address) => SwapAsset::Token { address: address.to_lowercase(), symbol: "WQI".into(), decimals: 18 },
                None => {
                    self.toast("no WQI on this network", true);
                    return true;
                }
            }
        } else {
            SwapAsset::Quai
        };
        let (from, to) = if buy { (counter, asset) } else { (asset, counter) };
        self.detail.clear();
        self.switch(Screen::Swap);
        let card = &mut self.eco.swap;
        card.from = from;
        card.to = Some(to);
        card.amount.clear();
        card.quote = None;
        card.preset = None;
        card.approving = false;
        card.field = 1;
        card.edited = Some(Instant::now());
        true
    }

    /// Candidate tokens for the picker: holdings first, then market tokens.
    /// The pool graph behind the picker's route badges. Empty until pools load, which
    /// `RouteState::Unknown` handles rather than filtering everything away.
    pub fn route_graph(&self) -> RouteGraph {
        let Some(Ok((pools, _))) = self.eco.markets_view.pools.as_ref().map(|r| r.as_ref()) else {
            return RouteGraph::default();
        };
        let hubs: Vec<String> = self
            .config
            .network(&self.network_id)
            .ok()
            .map(|n| {
                [n.wquai.clone(), n.wqi.clone(), n.ecosystem.usdt.as_ref().map(|u| u.address.clone())]
                    .into_iter()
                    .flatten()
                    .map(|a| a.to_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        RouteGraph::new(pools, &hubs)
    }

    /// The address a swap side occupies in the pool graph: native QUAI trades as WQUAI.
    pub(crate) fn graph_address(&self, asset: &SwapAsset) -> Option<String> {
        match asset {
            SwapAsset::Quai => self.net().and_then(|n| n.wquai.clone()).map(|a| a.to_lowercase()),
            SwapAsset::Token { address, .. } => Some(address.to_lowercase()),
        }
    }

    /// Token picker rows. `pay` picks which side is being chosen, so each row can say whether the
    /// router could actually fill it against the *other* side.
    pub fn picker_entries(&self, query: &str, pay: bool) -> Vec<PickerEntry> {
        let q = query.to_lowercase();
        let network = self.net();
        let graph = self.route_graph();
        // The side that is staying put. Picking the pay side is judged against the receive side.
        let counterpart =
            if pay { self.eco.swap.to.clone() } else { Some(self.eco.swap.from.clone()) }.and_then(|a| self.graph_address(&a));
        let route_for = |asset: &SwapAsset| -> RouteState {
            if graph.is_empty() {
                return RouteState::Unknown;
            }
            let (Some(other), Some(this)) = (counterpart.as_deref(), self.graph_address(asset)) else {
                return RouteState::Unknown;
            };
            // The same token on both sides is a wrap or a no-op, not a dead route; the card
            // already refuses it with a clearer message than the picker could.
            if this == other {
                return RouteState::Unknown;
            }
            let (from, to) = if pay { (this.as_str(), other) } else { (other, this.as_str()) };
            match graph.route(from, to) {
                Some(info) => RouteState::Fillable(info),
                None => RouteState::Dead,
            }
        };
        let curated: Vec<String> = network
            .as_ref()
            .map(|n| {
                [n.wqi.clone(), n.wquai.clone(), n.ecosystem.usdt.as_ref().map(|u| u.address.clone())]
                    .into_iter()
                    .flatten()
                    .map(|a| a.to_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        let row = |asset: SwapAsset, info: String, verified: bool, holders: Option<u64>, icon: Option<String>| PickerEntry {
            route: route_for(&asset),
            qi: false,
            asset,
            info,
            verified,
            holders,
            icon,
        };
        let mut out: Vec<PickerEntry> = vec![row(SwapAsset::Quai, "native".into(), true, None, None)];
        // Qi trades through the exchange's other routes: with QUAI (a conversion) and WQI (a wrap).
        let qi_info =
            self.dash.qi.as_ref().map_or_else(|| "private".into(), |q| format!("bal {}", super::super::num::qi(q.balance.spendable)));
        out.push(PickerEntry {
            qi: true,
            asset: SwapAsset::Quai,
            info: qi_info,
            verified: true,
            holders: None,
            icon: None,
            route: RouteState::Unknown,
        });
        let mut seen = std::collections::HashSet::new();
        if let Some(p) = &self.eco.portfolio {
            for r in &p.rows {
                if let AssetKey::Token(address) = &r.key
                    && seen.insert(address.clone())
                {
                    let bal = format!("bal {}", amount::group_thousands(&amount::format_amount_short(r.amount(), r.decimals, 4)));
                    out.push(row(
                        SwapAsset::Token { address: address.clone(), symbol: r.symbol.clone(), decimals: r.decimals },
                        bal,
                        r.trust == wallet_core::portfolio::Trust::Verified,
                        r.holders,
                        r.icon_url.clone(),
                    ));
                }
            }
        }
        for m in &self.eco.markets {
            if seen.insert(m.address.clone()) {
                let usdt =
                    network.as_ref().and_then(|n| n.ecosystem.usdt.as_ref()).is_some_and(|u| u.address.eq_ignore_ascii_case(&m.address));
                let decimals = match self.eco.token_info.get(&m.address) {
                    Some(Ok((info, _))) => info.decimals.unwrap_or(UNKNOWN_DECIMALS),
                    _ if usdt => 6,
                    _ if curated.contains(&m.address) => 18,
                    _ => UNKNOWN_DECIMALS,
                };
                // WQUAI is QUAI, priced as the rest of the app prices it; the token market list
                // carries no price for it.
                let price = m.price_usd.or_else(|| {
                    self.token_usd(&wallet_core::markets::PoolToken { address: m.address.clone(), symbol: m.symbol.clone(), decimals })
                });
                out.push(row(
                    SwapAsset::Token { address: m.address.clone(), symbol: m.symbol.clone(), decimals },
                    price.map(amount::usd_price).unwrap_or_else(|| "unpriced".into()),
                    curated.contains(&m.address),
                    m.holders,
                    m.icon_url.clone(),
                ));
            }
        }
        // Tokens that exist only in a pool: the explorer's market list does not carry every one,
        // and a token with a pool is by definition swappable, so it belongs in the picker.
        if let Some(Ok((pools, _))) = self.eco.markets_view.pools.as_ref().map(|r| r.as_ref()) {
            for token in pools.iter().flat_map(|p| [&p.token0, &p.token1]) {
                if !token.address.is_empty() && seen.insert(token.address.clone()) {
                    out.push(row(
                        SwapAsset::Token { address: token.address.clone(), symbol: token.symbol.clone(), decimals: token.decimals },
                        "in a pool".into(),
                        curated.contains(&token.address),
                        None,
                        None,
                    ));
                }
            }
        }
        // Qi reaches only QUAI (a conversion) and WQI (a wrap), each way; nothing else pairs
        // with it.
        let wqi = network.as_ref().and_then(|n| n.wqi.clone()).map(|a| a.to_lowercase());
        let pairs_with_qi = |a: &SwapAsset| match a {
            SwapAsset::Quai => true,
            SwapAsset::Token { address, .. } => wqi.as_deref() == Some(address.to_lowercase().as_str()),
        };
        let (here, there) = self.exchange_pair();
        let other = if pay { there } else { Some(here) };
        for e in &mut out {
            match (&other, e.qi) {
                // Picking the side's own counterpart: the sides swap.
                (Some(ExAsset::Qi), true) => e.route = RouteState::Unknown,
                (Some(ExAsset::Qi), false) => {
                    e.route = if pairs_with_qi(&e.asset) { RouteState::Unknown } else { RouteState::Dead };
                }
                // Choosing Qi always works: the other side becomes QUAI when it cannot pair.
                (Some(ExAsset::Swap(_)), true) => e.route = RouteState::Unknown,
                _ => {}
            }
        }
        out.retain(|e| {
            q.is_empty()
                || (e.qi && "qi".contains(&q))
                || (!e.qi && e.asset.symbol().to_lowercase().contains(&q))
                || matches!(&e.asset, SwapAsset::Token { address, .. } if address.contains(&q))
        });
        // What was typed ranks first: the exact symbol, then one that starts with it, then one that
        // contains it, then an address. Typing `qi` used to put WQI above Qi, and enter took it.
        // Within that, fillable first, then unknown; a token that cannot be reached is still listed
        // (so search finds it and says why) but never sits above one that can.
        let matched = |e: &PickerEntry| {
            let symbol = if e.qi { "qi".to_string() } else { e.asset.symbol().to_lowercase() };
            if q.is_empty() || symbol == q {
                0
            } else if symbol.starts_with(&q) {
                1
            } else if symbol.contains(&q) {
                2
            } else {
                3
            }
        };
        out.sort_by_key(|e| {
            let route = match &e.route {
                RouteState::Fillable(info) if !info.thin() => 0,
                RouteState::Fillable(_) => 1,
                RouteState::Unknown => 2,
                RouteState::Dead => 3,
            };
            (route == 3, matched(e), route)
        });
        out
    }

    /// The balance of a swap side, only when the wallet knows it exactly. An indexer's rounded
    /// figure is refused rather than filled: MAX from a rounded-up balance builds a transaction
    /// that reverts for insufficient funds.
    pub(crate) fn exact_balance(&self, asset: &SwapAsset) -> Option<(U256, u8)> {
        let rows = &self.eco.portfolio.as_ref()?.rows;
        let row = rows
            .iter()
            .find(|r| match (&r.key, asset) {
                (AssetKey::Quai, SwapAsset::Quai) => true,
                (AssetKey::Token(a), SwapAsset::Token { address, .. }) => a.eq_ignore_ascii_case(address),
                _ => false,
            })
            .filter(|r| r.exact)?;
        Some((row.amount(), row.decimals))
    }

    /// A configured wrapper token as a swap asset, so MAX can look its balance up.
    pub(crate) fn wrapper_asset(&self, wqi: bool) -> Option<SwapAsset> {
        let n = self.net()?;
        let address = if wqi { n.wqi.clone()? } else { n.wquai.clone()? };
        Some(SwapAsset::Token { address: address.to_lowercase(), symbol: if wqi { "WQI".into() } else { "WQUAI".into() }, decimals: 18 })
    }

    /// Qi coins this wallet could actually spend now: not reserved by a pending operation, and
    /// past their unlock height.
    pub(crate) fn spendable_coins(&self) -> Vec<wallet_core::spendable::Coin> {
        let Some(qi) = self.dash.qi.as_ref() else { return Vec::new() };
        let head = U256::from(qi.checkpoint_height.unwrap_or(u64::MAX));
        qi.coins
            .iter()
            .filter(|c| !c.reserved && c.unlock_height <= head)
            .map(|c| wallet_core::spendable::Coin { qits: c.qits, denomination: c.denomination })
            .collect()
    }

    pub fn fill_max(&mut self) {
        if (self.screen == Screen::Convert && self.eco.convert.qi_to_quai) || (self.screen == Screen::Wrap && self.eco.wrap.mode == 0) {
            self.eco.max_sequence = self.eco.max_sequence.wrapping_add(1);
            let key = self.eco.max_sequence;
            self.eco.max_request = Some((key, self.max_identity()));
            self.send(Cmd::QiMax {
                key,
                wrapping: self.screen == Screen::Wrap,
                account: self.dash.accounts.first().map(|a| a.address.clone()),
                slippage: self.eco.convert.slippage_bps,
            });
            self.info("quoting a spendable Qi amount with current fees…");
            return;
        }
        use wallet_core::spendable::{MAX_ROUTE_HOPS, qi_max, quai_max, swap_max_gas, token_max};
        // Which card is being edited, and what it pays with.
        enum Pay {
            Asset(SwapAsset),
            Qi { whole: bool },
        }
        let pay = match self.screen {
            Screen::Swap => Pay::Asset(self.eco.swap.from.clone()),
            // Qi → QUAI spends Qi coins. The protocol conversion takes fractional Qi
            // (`review_convert_qi_to_quai` parses 3 decimals), so MAX must not round down.
            Screen::Convert if self.eco.convert.qi_to_quai => Pay::Qi { whole: false },
            Screen::Convert => Pay::Asset(SwapAsset::Quai),
            Screen::Wrap => match self.eco.wrap.mode {
                // Wrapping takes fractional Qi too; only redemption (mode 2) is whole-Qi, and that
                // side spends WQI, handled as a token below.
                0 => Pay::Qi { whole: false },
                // Claim takes no amount at all.
                1 => return,
                // WQI → Qi and WQUAI → QUAI spend the wrapper token, an ordinary ERC-20.
                2 => match self.wrapper_asset(true) {
                    Some(a) => Pay::Asset(a),
                    None => return,
                },
                3 => Pay::Asset(SwapAsset::Quai),
                _ => match self.wrapper_asset(false) {
                    Some(a) => Pay::Asset(a),
                    None => return,
                },
            },
            _ => return,
        };
        let (text, note) = match pay {
            Pay::Qi { whole } => {
                let coins = self.spendable_coins();
                if coins.is_empty() {
                    self.toast("no spendable Qi coins yet", true);
                    return;
                }
                let m = qi_max(&coins, whole);
                if m.amount.is_zero() {
                    self.toast("the spendable Qi coins do not cover a whole Qi plus its fee", true);
                    return;
                }
                (m.text(), m.note())
            }
            Pay::Asset(asset) => {
                let Some((balance, decimals)) = self.exact_balance(&asset) else {
                    self.toast("balance is still loading", true);
                    return;
                };
                if balance.is_zero() {
                    self.toast(format!("no {} to spend", asset.symbol()), true);
                    return;
                }
                match asset {
                    SwapAsset::Quai => {
                        let Some(price) = self.eco.gas_price else {
                            self.toast("waiting for the gas price before MAX can keep fees back", true);
                            return;
                        };
                        // Budget for the deepest route: the receive token can still change.
                        let m = quai_max(balance, price, swap_max_gas(MAX_ROUTE_HOPS));
                        if m.is_zero() {
                            self.toast("this balance cannot cover its own fee", true);
                            return;
                        }
                        (m.text(), m.note())
                    }
                    SwapAsset::Token { .. } => {
                        let mut m = token_max(balance, decimals);
                        // Redeeming WQI pays out whole Qi, so offering a fractional MAX would
                        // only be rounded away by the contract.
                        if self.screen == Screen::Wrap && self.eco.wrap.mode == 2 {
                            let unit = U256::from(10u64).pow(U256::from(decimals));
                            m.amount -= m.amount % unit;
                            if m.amount.is_zero() {
                                self.toast("less than one whole WQI to redeem", true);
                                return;
                            }
                        }
                        (m.text(), m.note())
                    }
                }
            }
        };
        match self.screen {
            Screen::Swap => {
                self.eco.swap.amount = text;
                self.eco.swap.field = 1;
                self.eco.swap.edited = Some(Instant::now());
                self.eco.swap.requested_key = 0;
                self.eco.swap.approving = false;
            }
            Screen::Convert => {
                self.eco.convert.amount = text;
                self.eco.convert.field = 1;
                self.eco.convert.edited = Some(Instant::now());
            }
            Screen::Wrap => {
                self.eco.wrap.amount = text;
                self.eco.wrap.field = 1;
            }
            _ => return,
        }
        if let Some(note) = note {
            self.toast(&note, false);
        }
    }

    /// Keys for views with inline inputs. Returns true when consumed.
    pub fn view_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }
        match self.screen {
            Screen::Markets => self.markets_key(key),
            Screen::Board => self.board_key(key),
            Screen::Pools => self.pools_key(key),
            Screen::Launches => self.launches_key(key),
            Screen::Pnl => self.pnl_key(key),
            Screen::Swap => self.swap_key(key),
            Screen::Convert => self.convert_key(key),
            Screen::Wrap => self.wrap_key(key),
            Screen::Explore => self.explore_key(key),
            Screen::Collected => match key.code {
                KeyCode::Char('h') | KeyCode::Left => {
                    self.move_selection(-1);
                    true
                }
                KeyCode::Char('l') | KeyCode::Right if self.eco.nft_len() > 0 => {
                    self.move_selection(1);
                    true
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    let cols = (*self.eco.grid_columns.borrow()).max(1);
                    let len = self.eco.nft_len();
                    if len > 0 {
                        self.selected = (self.selected + cols).min(len - 1);
                    }
                    true
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    let cols = (*self.eco.grid_columns.borrow()).max(1);
                    self.selected = self.selected.saturating_sub(cols);
                    true
                }
                KeyCode::Char('R') => {
                    self.load_nfts(true);
                    true
                }
                KeyCode::Char('T') => {
                    self.transfer_selected_nft();
                    true
                }
                KeyCode::Char('L') => {
                    if let Some((c, id)) = self.selected_nft() {
                        self.open_nft_list(&c, &id);
                    }
                    true
                }
                KeyCode::Char('X') => {
                    if let Some((c, id)) = self.selected_nft() {
                        self.cancel_nft_listing(&c, &id);
                    }
                    true
                }
                _ => false,
            },
            Screen::Listings => match key.code {
                KeyCode::Char('m') => {
                    self.eco.listings_mine = !self.eco.listings_mine;
                    self.selected = 0;
                    if self.eco.listings_mine {
                        self.load_my_listings();
                    }
                    self.toast(if self.eco.listings_mine { "listings: yours" } else { "listings: everyone's" }, false);
                    true
                }
                KeyCode::Char('S') => {
                    self.eco.listing_sort = self.eco.listing_sort.next();
                    self.selected = 0;
                    let label = self.eco.listing_sort.label();
                    self.info(format!("listings: {label}"));
                    true
                }
                KeyCode::Char(c @ ('f' | 'F')) => {
                    let collections = match self.eco.listings.get(&None) {
                        Some(Ok(all)) => wallet_core::market::listing_collections(all),
                        _ => Vec::new(),
                    };
                    if collections.is_empty() {
                        return true;
                    }
                    // Positions: 0 = all collections, then each collection by listing count.
                    let len = collections.len() + 1;
                    let current = self
                        .eco
                        .listing_filter
                        .as_ref()
                        .and_then(|f| collections.iter().position(|(a, _)| a == f).map(|p| p + 1))
                        .unwrap_or(0);
                    let next = if c == 'f' { (current + 1) % len } else { (current + len - 1) % len };
                    self.eco.listing_filter = (next > 0).then(|| collections[next - 1].0.clone());
                    self.selected = 0;
                    let text = match &self.eco.listing_filter {
                        Some(a) => format!("listings: {} ({} listed)", self.eco.collection_name(a), collections[next - 1].1),
                        None => "listings: all collections".into(),
                    };
                    self.toast(text, false);
                    true
                }
                KeyCode::Char('R') => {
                    self.eco.listings_loading = true;
                    self.send_data(DataCmd::Listings { collection: None });
                    true
                }
                KeyCode::Char('b') => {
                    if let Some(l) = self.eco.visible_listings().get(self.selected).cloned() {
                        self.push_detail(Detail::Nft(l.contract.clone(), l.token_id.clone()));
                        self.buy_listing(&l);
                    }
                    true
                }
                _ => false,
            },
            Screen::Home if key.code == KeyCode::Char('i') => {
                self.eco.info_open = !self.eco.info_open;
                true
            }
            Screen::Network if key.code == KeyCode::Char('m') => {
                if let Some((id, _)) = self.dash.networks.get(self.selected).cloned() {
                    self.open_form(super::super::app::FormKind::Monitor { network: id });
                }
                true
            }
            _ => false,
        }
    }

    pub(crate) fn swap_key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Char('B') {
            let asset = |a: &SwapAsset| match a {
                SwapAsset::Quai => "quai".into(),
                SwapAsset::Token { address, .. } => address.clone(),
            };
            if let Some(to) = &self.eco.swap.to {
                self.open_form(FormKind::BoundedSwap {
                    from: asset(&self.eco.swap.from),
                    to: asset(to),
                    input: self.eco.swap.amount.clone(),
                });
            } else {
                self.toast("choose a receive token first", true);
            }
            return true;
        }
        if key.code == KeyCode::Char('L') {
            self.switch(Screen::Orders);
            return true;
        }
        if key.code == KeyCode::Char('O') {
            let asset = |a: &SwapAsset| match a {
                SwapAsset::Quai => "quai".into(),
                SwapAsset::Token { address, .. } => address.clone(),
            };
            // The order is this swap, waited for: it needs the pair, the amount and the quote the
            // card is showing, which is what the form measures a target against.
            let quote = match (&self.eco.swap.to, &self.eco.swap.quote) {
                (None, _) => return self.toast_and_consume("choose what to receive first"),
                (_, _) if self.eco.swap.amount.trim().is_empty() => return self.toast_and_consume("type an amount to trade first"),
                (Some(_), Some(Ok(q))) if q.from == self.eco.swap.from && Some(&q.to) == self.eco.swap.to.as_ref() => q.clone(),
                _ => return self.toast_and_consume("wait for the quote, then create the order"),
            };
            let preview = super::super::order_ui::Preview {
                from_symbol: quote.from.symbol().to_string(),
                to_symbol: quote.to.symbol().to_string(),
                from_decimals: quote.from.decimals(),
                to_decimals: quote.to.decimals(),
                input_atoms: quote.amount_in.clone(),
                current_out: quote.amount_out.clone(),
            };
            self.open_form(FormKind::OrderCreate {
                from: asset(&quote.from),
                to: asset(&quote.to),
                input: self.eco.swap.amount.clone(),
                slippage: self.eco.swap.slippage_bps,
                preview: Box::new(preview),
            });
            return true;
        }
        if key.code == KeyCode::Char('P') {
            let asset = |a: &SwapAsset| match a {
                SwapAsset::Quai => "quai".into(),
                SwapAsset::Token { address, .. } => address.clone(),
            };
            if let Some(identity) = self.swap_input_key()
                && let Some(to) = &self.eco.swap.to
            {
                self.eco.max_sequence = self.eco.max_sequence.wrapping_add(1);
                let key = self.eco.max_sequence;
                self.eco.split_request = Some((key, identity));
                self.send(Cmd::SplitQuote {
                    key,
                    account: self.dash.accounts.first().map(|a| a.address.clone()),
                    from: asset(&self.eco.swap.from),
                    to: asset(to),
                    amount: self.eco.swap.amount.clone(),
                    slippage: self.eco.swap.slippage_bps,
                });
                self.info("comparing split allocations and added fees…");
            } else {
                self.toast("choose both tokens and enter an amount first", true);
            }
            return true;
        }
        if key.code == KeyCode::Char('E') {
            let asset = |a: &SwapAsset| match a {
                SwapAsset::Quai => "quai".into(),
                SwapAsset::Token { address, .. } => address.clone(),
            };
            if let Some(to) = &self.eco.swap.to {
                self.open_form(FormKind::ExactOutput { from: asset(&self.eco.swap.from), to: asset(to) });
            } else {
                self.toast("choose a receive token first", true);
            }
            return true;
        }
        // MAX is about the balance, not the cursor, so it works focused or not.
        if key.code == KeyCode::Char('m') {
            self.fill_max();
            self.eco.swap.preset = if self.eco.swap.amount.is_empty() { None } else { Some(100) };
            return true;
        }
        // A quarter, half, three quarters, all: each press takes the next share of MAX.
        if key.code == KeyCode::Char('%') {
            self.fill_share();
            return true;
        }
        let card = &mut self.eco.swap;
        if card.field == 5 {
            // Unfocused: only Tab / Enter / picker / flip enter the card; everything else is global.
            match key.code {
                KeyCode::Tab | KeyCode::Enter => card.field = 1,
                KeyCode::BackTab => card.field = 4,
                KeyCode::Char('/') | KeyCode::Char('f') => {}
                _ => return false,
            }
            if matches!(key.code, KeyCode::Tab | KeyCode::Enter | KeyCode::BackTab) {
                return true;
            }
        }
        match key.code {
            KeyCode::Esc => card.field = 5,
            KeyCode::Tab => card.field = (card.field + 1) % 5,
            KeyCode::BackTab => card.field = (card.field + 4) % 5,
            KeyCode::Down if card.field < 4 => card.field += 1,
            KeyCode::Up if card.field > 0 => card.field -= 1,
            KeyCode::Char('/') => {
                let pay = card.field != 2;
                self.modal = Modal::TokenPicker { pay, query: String::new(), selected: 0 };
            }
            KeyCode::Char('f') => {
                if let Some(to) = card.to.take() {
                    card.to = Some(std::mem::replace(&mut card.from, to));
                    card.amount.clear();
                    card.quote = None;
                    card.edited = Some(Instant::now());
                    card.requested_key = 0;
                }
            }
            KeyCode::Left | KeyCode::Right if card.field == 3 => {
                let steps = [10u16, 30, 50, 100, 200, 300, 500, 1000];
                let i = steps.iter().position(|s| *s >= card.slippage_bps).unwrap_or(2) as i32;
                let d = if key.code == KeyCode::Left { -1 } else { 1 };
                card.slippage_bps = steps[(i + d).clamp(0, steps.len() as i32 - 1) as usize];
                card.edited = Some(Instant::now());
                card.requested_key = 0;
            }
            KeyCode::Left | KeyCode::Right if card.field == 4 => {
                let steps = [2u32, 5, 10, 20, 30, 60];
                let i = steps.iter().position(|s| *s >= card.deadline_minutes).unwrap_or(2) as i32;
                let d = if key.code == KeyCode::Left { -1 } else { 1 };
                card.deadline_minutes = steps[(i + d).clamp(0, steps.len() as i32 - 1) as usize];
                card.requested_key = 0;
                card.edited = Some(Instant::now());
            }
            KeyCode::Enter if matches!(card.field, 0 | 2) => {
                let pay = card.field == 0;
                self.modal = Modal::TokenPicker { pay, query: String::new(), selected: 0 };
            }
            KeyCode::Enter => self.swap_submit(),
            _ if card.field == 1 => {
                let decimals = card.from.decimals();
                if digits_input(&mut card.amount, &key, decimals) {
                    card.field = 1;
                    card.edited = Some(Instant::now());
                    card.requested_key = 0;
                    card.approving = false;
                    card.preset = None;
                    return true;
                }
                return false;
            }
            _ => return false,
        }
        true
    }

    /// The next of 25/50/75/100% of what MAX would fill, on the swap card.
    pub(crate) fn fill_share(&mut self) {
        let next = match self.eco.swap.preset {
            Some(25) => 50,
            Some(50) => 75,
            Some(75) => 100,
            _ => 25,
        };
        self.fill_max();
        let card = &mut self.eco.swap;
        let decimals = card.from.decimals();
        let Ok(max) = amount::parse_amount(&card.amount, decimals) else { return };
        if max.is_zero() {
            return;
        }
        let share = max * U256::from(next) / U256::from(100u8);
        card.amount = amount::format_amount(share, decimals);
        card.preset = Some(next);
        card.edited = Some(Instant::now());
    }

    /// Watched pools first, in the order they were watched; the rest keep their order. The cursor
    /// stays on the pool it was on.
    /// Put the cursor back on `address` after the list is re-ordered (a pair watched or
    /// unwatched, a refreshed directory). [`App::market_rows`] decides the order itself, so
    /// nothing here moves the pools.
    pub fn keep_cursor_on(&mut self, address: Option<String>) {
        let Some(addr) = address else { return };
        let Some(i) = self.market_rows().iter().position(|p| p.address == addr) else { return };
        if self.screen == Screen::Markets && self.pane == 0 {
            self.selected = i;
        }
        self.eco.markets_view.pair_selected = i;
    }

    /// Picker choice.
    pub fn pick_swap_asset(&mut self, pay: bool, asset: SwapAsset) {
        if let SwapAsset::Token { address, decimals: UNKNOWN_DECIMALS, .. } = &asset {
            self.send_data(DataCmd::TokenInfo(address.clone()));
        }
        let card = &mut self.eco.swap;
        if pay {
            if card.to.as_ref() == Some(&asset) {
                card.to = Some(card.from.clone());
            }
            card.from = asset;
            card.amount.clear();
        } else {
            if card.from == asset {
                card.from = card.to.clone().unwrap_or(SwapAsset::Quai);
            }
            card.to = Some(asset);
        }
        card.quote = None;
        card.approving = false;
        card.edited = Some(Instant::now());
        card.field = 1;
    }

    /// The exchange's pair, read off whichever of its views is showing (or was last): what is
    /// paid, and what is received (a swap may not have chosen yet).
    pub fn exchange_pair(&self) -> (ExAsset, Option<ExAsset>) {
        let net = self.net();
        let token = |address: Option<String>, symbol: &str| {
            ExAsset::Swap(address.map_or(SwapAsset::Quai, |address| SwapAsset::Token {
                address: address.to_lowercase(),
                symbol: symbol.into(),
                decimals: 18,
            }))
        };
        let wqi = || token(net.as_ref().and_then(|n| n.wqi.clone()), "WQI");
        let wquai = || token(net.as_ref().and_then(|n| n.wquai.clone()), "WQUAI");
        let quai = ExAsset::Swap(SwapAsset::Quai);
        let view = if self.screen.is_exchange() { self.screen } else { self.last_exchange };
        match view {
            Screen::Convert if self.eco.convert.qi_to_quai => (ExAsset::Qi, Some(quai)),
            Screen::Convert => (quai, Some(ExAsset::Qi)),
            Screen::Wrap => match self.eco.wrap.mode {
                0 | 1 => (ExAsset::Qi, Some(wqi())),
                2 => (wqi(), Some(ExAsset::Qi)),
                3 => (quai, Some(wquai())),
                _ => (wquai(), Some(quai)),
            },
            _ => (ExAsset::Swap(self.eco.swap.from.clone()), self.eco.swap.to.clone().map(ExAsset::Swap)),
        }
    }

    /// Choose one side of the exchange. The pair then decides which of the exchange's views
    /// carries it: QUAI and Qi convert, Qi and WQI or QUAI and WQUAI wrap, anything else swaps.
    /// Choosing the other side's asset turns the pair around.
    pub fn pick_exchange(&mut self, pay: bool, asset: ExAsset) {
        let (here, there) = self.exchange_pair();
        let chose_qi = asset == ExAsset::Qi;
        let (mut from, mut to) = if pay { (asset, there) } else { (here.clone(), Some(asset)) };
        if to.as_ref() == Some(&from) {
            if pay {
                to = Some(here);
            } else if let Some(t) = self.exchange_pair().1 {
                from = t;
            }
        }
        // Qi pairs with QUAI and WQI only: choosing it opposite anything else makes it a
        // conversion, and says so.
        if chose_qi {
            let net = self.net();
            let wqi = net.as_ref().and_then(|n| n.wqi.clone());
            let pairs = |a: &Option<ExAsset>| match a {
                Some(ExAsset::Swap(SwapAsset::Quai)) => true,
                Some(ExAsset::Swap(SwapAsset::Token { address, .. })) => wqi.as_ref().is_some_and(|w| w.eq_ignore_ascii_case(address)),
                _ => false,
            };
            let other = if pay { to.clone() } else { Some(from.clone()) };
            if !pairs(&other) {
                let was = other.as_ref().map(|o| o.symbol().to_string()).unwrap_or_default();
                if pay {
                    to = Some(ExAsset::Swap(SwapAsset::Quai));
                } else {
                    from = ExAsset::Swap(SwapAsset::Quai);
                }
                if !was.is_empty() {
                    self.info(format!("Qi trades with QUAI and WQI · {was} became QUAI, a conversion"));
                }
            }
        }
        self.route_exchange(from, to);
    }

    fn route_exchange(&mut self, from: ExAsset, to: Option<ExAsset>) {
        let net = self.net();
        let is = |a: &ExAsset, address: Option<&String>| matches!((a, address), (ExAsset::Swap(SwapAsset::Token { address: x, .. }), Some(y)) if x.eq_ignore_ascii_case(y));
        let (wqi, wquai) = (net.as_ref().and_then(|n| n.wqi.clone()), net.as_ref().and_then(|n| n.wquai.clone()));
        let quai = ExAsset::Swap(SwapAsset::Quai);
        // What was typed follows when the side it was typed for is still the side paid.
        let (was, _) = self.exchange_pair();
        let typed = match (self.screen, was == from) {
            (_, false) => String::new(),
            (Screen::Convert, _) => self.eco.convert.amount.clone(),
            (Screen::Wrap, _) => self.eco.wrap.amount.clone(),
            _ => self.eco.swap.amount.clone(),
        };
        let wrap = |app: &mut App, mode: usize| {
            app.eco.wrap.mode = mode;
            app.eco.wrap.amount = typed.clone();
            app.switch(Screen::Wrap);
            app.eco.wrap.field = 1;
        };
        match (&from, &to) {
            (f, Some(ExAsset::Qi)) if *f == quai => {
                self.eco.convert.qi_to_quai = false;
                self.set_convert_amount(typed);
            }
            (ExAsset::Qi, Some(t)) if *t == quai => {
                self.eco.convert.qi_to_quai = true;
                self.set_convert_amount(typed);
            }
            (ExAsset::Qi, Some(t)) if is(t, wqi.as_ref()) => wrap(self, 0),
            (f, Some(ExAsset::Qi)) if is(f, wqi.as_ref()) => wrap(self, 2),
            (f, Some(t)) if *f == quai && is(t, wquai.as_ref()) => wrap(self, 3),
            (f, Some(t)) if is(f, wquai.as_ref()) && *t == quai => wrap(self, 4),
            (ExAsset::Qi, _) | (_, Some(ExAsset::Qi)) => {
                self.toast("Qi trades with QUAI (a conversion) and WQI (a wrap); for anything else, convert to QUAI first", true);
            }
            (ExAsset::Swap(f), t) => {
                if !self.config.features.on(wallet_core::config::Feature::Trading) {
                    self.info("swapping tokens is part of trading · System › Settings turns it on");
                    return;
                }
                let t = t.as_ref().and_then(|t| match t {
                    ExAsset::Swap(a) => Some(a.clone()),
                    ExAsset::Qi => None,
                });
                if self.eco.swap.from != *f {
                    self.pick_swap_asset(true, f.clone());
                }
                if let Some(t) = t
                    && self.eco.swap.to.as_ref() != Some(&t)
                {
                    self.pick_swap_asset(false, t);
                }
                if !typed.is_empty() {
                    self.eco.swap.amount = typed;
                }
                self.switch(Screen::Swap);
                self.eco.swap.field = 1;
            }
        }
    }

    fn set_convert_amount(&mut self, typed: String) {
        let card = &mut self.eco.convert;
        card.amount = typed;
        card.quote = None;
        card.routes = None;
        self.switch(Screen::Convert);
        self.eco.convert.field = 1;
    }

    /// Say why a key did nothing, and consume it.
    fn toast_and_consume(&mut self, text: &str) -> bool {
        self.toast(text, true);
        true
    }

    /// Esc on Convert or Wrap: back to the swap card, on the pair it last had. Choosing Qi (or a
    /// wrapped asset opposite its own) turns the exchange into a conversion or a wrap, and this is
    /// the way back to a market swap.
    pub(crate) fn exchange_back_to_swap(&mut self) {
        // Without trading there is no swap card to go back to, and Esc says nothing.
        if self.config.features.on(wallet_core::config::Feature::Trading) {
            self.switch(Screen::Swap);
        }
    }

    /// `/` on Convert and Wrap: the exchange's picker, for what to receive.
    pub(crate) fn open_exchange_picker(&mut self) {
        self.modal = Modal::TokenPicker { pay: false, query: String::new(), selected: 0 };
    }

    /// Unfocus exchange cards when their view is opened by navigation.
    pub fn unfocus_cards(&mut self) {
        self.eco.swap.field = 5;
        self.eco.convert.field = 4;
        self.eco.wrap.field = 2;
    }

    pub(crate) fn convert_key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Char('m') {
            self.fill_max();
            return true;
        }
        let card = &mut self.eco.convert;
        if card.field >= 4 {
            match key.code {
                KeyCode::Tab | KeyCode::Enter => card.field = 1,
                KeyCode::BackTab => card.field = 3,
                KeyCode::Char('f') => {
                    card.qi_to_quai = !card.qi_to_quai;
                    card.amount.clear();
                    card.quote = None;
                    card.routes = None;
                }
                KeyCode::Char('r') => card.market = !card.market,
                _ => return false,
            }
            return true;
        }
        match key.code {
            KeyCode::Esc => card.field = 4,
            KeyCode::Tab => card.field = (card.field + 1) % 4,
            KeyCode::BackTab => card.field = (card.field + 3) % 4,
            KeyCode::Down if card.field < 3 => card.field += 1,
            KeyCode::Up if card.field > 0 => card.field -= 1,
            KeyCode::Char('f') => {
                card.qi_to_quai = !card.qi_to_quai;
                card.amount.clear();
                card.quote = None;
                card.routes = None;
            }
            KeyCode::Char('r') => card.market = !card.market,
            KeyCode::Left | KeyCode::Right if card.field == 0 => {
                card.qi_to_quai = !card.qi_to_quai;
                card.amount.clear();
                card.quote = None;
                card.routes = None;
            }
            KeyCode::Left | KeyCode::Right if card.field == 3 => card.market = !card.market,
            KeyCode::Left | KeyCode::Right if card.field == 2 => {
                // Up to the 9000 the node clamps to: a saturated conversion needs the maximum, and
                // before this the arrows could not reach past 20%.
                let steps = [50u16, 100, 200, 300, 500, 1000, 2000, 3000, 5000, 9000];
                let i = steps.iter().position(|s| *s >= card.slippage_bps).unwrap_or(3) as i32;
                let d = if key.code == KeyCode::Left { -1 } else { 1 };
                card.slippage_bps = steps[(i + d).clamp(0, steps.len() as i32 - 1) as usize];
                card.manual_slippage = true;
            }
            KeyCode::Enter => {
                let direction = if card.qi_to_quai { "qi_to_quai" } else { "quai_to_qi" };
                let (qi_to_quai, amount, slippage) =
                    (card.qi_to_quai, card.amount.clone(), card.manual_slippage.then_some(card.slippage_bps));
                if amount.is_empty() {
                    self.toast("enter an amount", true);
                    return true;
                }
                // The market route: wrap, swap and unwrap, each step reviewed.
                if card.market {
                    let usable = match &card.routes {
                        Some(Ok(c)) => c.market.usable(),
                        _ => false,
                    };
                    if !usable {
                        self.toast("no market route for this amount yet", true);
                        return true;
                    }
                    if !self.can_sign() {
                        self.toast("this wallet is watch-only", true);
                        return true;
                    }
                    let direction =
                        wallet_core::qi_market::Direction::parse(direction).unwrap_or(wallet_core::qi_market::Direction::QuaiToQi);
                    let slippage = self.swap_slippage();
                    self.start_qi_route(direction, amount, slippage);
                    return true;
                }
                let current = card.quoted_for.as_ref() == Some(&(qi_to_quai, amount.clone())) && card.quote.is_some();
                if !current {
                    card.quoted_for = Some((qi_to_quai, amount.clone()));
                    card.quote = None;
                    self.send(Cmd::Quote { direction: direction.into(), amount });
                } else if !self.can_sign() {
                    self.toast("this wallet is watch-only", true);
                } else {
                    let account = self.dash.accounts.first().map(|a| a.address.clone());
                    let direction =
                        if qi_to_quai { wallet_core::qi_market::Direction::QiToQuai } else { wallet_core::qi_market::Direction::QuaiToQi };
                    self.start_protocol_conversion(direction, amount, slippage, account);
                }
            }
            _ if card.field != 1 => return false,
            _ => {
                let decimals = if card.qi_to_quai { 3 } else { 18 };
                if digits_input(&mut card.amount, &key, decimals) {
                    card.field = 1;
                    card.quote = None;
                    return true;
                }
                return false;
            }
        }
        true
    }

    pub(crate) fn wrap_key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Char('m') && self.eco.wrap.mode != 1 {
            self.fill_max();
            return true;
        }
        let card = &mut self.eco.wrap;
        if card.field >= 2 {
            match key.code {
                KeyCode::Tab | KeyCode::Enter => card.field = if card.mode == 1 { 0 } else { 1 },
                KeyCode::BackTab => card.field = 0,
                _ => return false,
            }
            return true;
        }
        match key.code {
            KeyCode::Esc => card.field = 2,
            KeyCode::Tab | KeyCode::BackTab => card.field = 1 - card.field.min(1),
            KeyCode::Down if card.field == 0 => card.field = 1,
            KeyCode::Up if card.field == 1 => card.field = 0,
            KeyCode::Left | KeyCode::Right if card.field == 0 => {
                let d: i32 = if key.code == KeyCode::Left { -1 } else { 1 };
                card.mode = (card.mode as i32 + d).rem_euclid(WRAP_MODES.len() as i32) as usize;
                card.amount.clear();
            }
            KeyCode::Enter => {
                let (mode, amount) = (card.mode, card.amount.clone());
                if !self.can_sign() {
                    self.toast("this wallet is watch-only", true);
                    return true;
                }
                if mode != 1 && amount.is_empty() {
                    self.toast("enter an amount", true);
                    return true;
                }
                let account = self.dash.accounts.first().map(|a| a.address.clone());
                if mode == 1 {
                    self.claim_now(account);
                    return true;
                }
                let prepare = match mode {
                    0 => Prepare::WrapQi { account, amount },
                    2 => Prepare::UnwrapWqi { account, amount },
                    3 => Prepare::WrapQuai { account, amount },
                    _ => Prepare::UnwrapQuai { account, amount },
                };
                self.send(Cmd::Prepare(prepare));
            }
            _ => {
                if card.mode == 1 || card.field != 1 {
                    return false;
                }
                let decimals = if matches!(card.mode, 0 | 2) { 3 } else { 18 };
                if digits_input(&mut card.amount, &key, decimals) {
                    card.field = 1;
                    return true;
                }
                return false;
            }
        }
        true
    }

    /// Token picker keys. Returns the modal to keep open, if any.
    pub fn picker_key(&mut self, pay: bool, mut query: String, mut selected: usize, key: KeyEvent) -> Modal {
        let entries = self.picker_entries(&query, pay);
        match key.code {
            KeyCode::Esc => return Modal::None,
            KeyCode::Down | KeyCode::Tab => selected = (selected + 1).min(entries.len().saturating_sub(1)),
            KeyCode::Up | KeyCode::BackTab => selected = selected.saturating_sub(1),
            KeyCode::Enter => {
                match entries.get(selected) {
                    // Refusing here is the whole point: the alternative is letting the user build a
                    // pair and meeting them with "no Quainance pool route" after the fact.
                    Some(entry) if !entry.route.choosable() => {
                        let (here, there) = self.exchange_pair();
                        let other = if pay { there } else { Some(here) };
                        let other_name = other.as_ref().map(ExAsset::symbol).unwrap_or("the other side").to_string();
                        let this = if entry.qi { "Qi" } else { entry.asset.symbol() };
                        let why = if entry.qi || matches!(other, Some(ExAsset::Qi)) {
                            "Qi trades with QUAI (a conversion) and WQI (a wrap)".to_string()
                        } else {
                            format!("no pool route between {this} and {other_name}")
                        };
                        self.toast(why, true);
                        return Modal::TokenPicker { pay, query, selected };
                    }
                    Some(entry) => {
                        let asset = if entry.qi { ExAsset::Qi } else { ExAsset::Swap(entry.asset.clone()) };
                        self.pick_exchange(pay, asset);
                    }
                    None => {}
                }
                return Modal::None;
            }
            KeyCode::Backspace => {
                query.pop();
                selected = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) && query.len() < 42 => {
                query.push(c);
                selected = 0;
            }
            _ => {}
        }
        Modal::TokenPicker { pay, query, selected }
    }

    pub(crate) fn swap_input_key(&self) -> Option<u64> {
        let card = &self.eco.swap;
        let to = card.to.as_ref()?;
        let atoms = amount::parse_amount(&card.amount, card.from.decimals()).ok()?;
        Some(hash_key(&[
            &format!("{:?}", card.from),
            &format!("{to:?}"),
            &atoms.to_string(),
            &card.slippage_bps.to_string(),
            &card.deadline_minutes.to_string(),
            &self.network_id,
            &self.dash.accounts.first().map(|a| a.address.clone()).unwrap_or_default(),
        ]))
    }

    /// `m` on an amount field: fill it with everything that can actually be spent.
    ///
    /// Three ledgers, three answers — an ERC-20 spends its whole balance, native QUAI must keep
    /// back what the transaction costs, and Qi is capped by how many coins one transaction holds.
    pub(crate) fn max_identity(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}", self.screen).hash(&mut h);
        self.meta.as_ref().map(|m| &m.id).hash(&mut h);
        self.network_id.hash(&mut h);
        self.dash.accounts.first().map(|a| &a.address).hash(&mut h);
        self.eco.convert.amount.hash(&mut h);
        self.eco.convert.qi_to_quai.hash(&mut h);
        self.eco.convert.slippage_bps.hash(&mut h);
        self.eco.wrap.amount.hash(&mut h);
        self.eco.wrap.mode.hash(&mut h);
        h.finish()
    }

    /// The bonding curve that trades this pair's token against QUAI, from the market directory: a
    /// token still on its curve, or one bonded onto a pool its curve keeps. `None` for a pair
    /// without QUAI on one side, or a token without a curve.
    pub(crate) fn token_curve(&self, a: &SwapAsset, b: &SwapAsset) -> Option<(wallet_core::markets::PoolToken, String)> {
        let token = match (a, b) {
            (SwapAsset::Quai, SwapAsset::Token { address, .. }) | (SwapAsset::Token { address, .. }, SwapAsset::Quai) => address,
            _ => return None,
        };
        let Some(Ok((pools, _))) = self.eco.markets_view.pools.as_ref().map(|r| r.as_ref()) else { return None };
        pools
            .iter()
            .find(|p| p.venue == wallet_core::markets::Venue::Curve && p.token0.address.eq_ignore_ascii_case(token))
            .map(|p| (p.token0.clone(), p.address.clone()))
    }

    /// Where the swap card's trade goes: the token's curve when it pays more than the exchanges,
    /// or when the exchanges cannot fill it at all; otherwise the router. Only a curve quote for
    /// the pair and amount on the card counts.
    pub(crate) fn swap_uses_curve(&self) -> Option<wallet_core::curve::CurveOffer> {
        let card = &self.eco.swap;
        let offer = card.curve.as_ref()?.as_ref().ok()?;
        let to = card.to.as_ref()?;
        let atoms = amount::parse_amount(&card.amount, card.from.decimals()).ok()?;
        let token = if offer.sell { &card.from } else { to };
        let matches = offer.input == atoms.to_string()
            && matches!(token, SwapAsset::Token { address, .. } if address.eq_ignore_ascii_case(&offer.token))
            && matches!(if offer.sell { to } else { &card.from }, SwapAsset::Quai);
        let curve_out = offer.amount().filter(|v| !v.is_zero())?;
        if !matches {
            return None;
        }
        match &card.quote {
            Some(Ok(q)) if q.from == card.from && Some(&q.to) == card.to.as_ref() => {
                let routed = U256::from_str_radix(&q.amount_out, 10).unwrap_or(U256::ZERO);
                (curve_out > routed).then(|| offer.clone())
            }
            // The exchanges refused (no route, or impact past the limit): the curve fills it.
            Some(Err(_)) => Some(offer.clone()),
            _ => None,
        }
    }

    /// Keep the swap card's rate and chart fed: the market list, then the pair's hourly candles.
    pub(crate) fn tick_swap(&mut self) {
        let mv = &self.eco.markets_view;
        if !mv.pools_loading
            && mv.pools_attempted.is_none_or(|at| at.elapsed() >= MARKET_REFRESH)
            && (mv.pools.is_none() || mv.pools_at.is_none_or(|t| t.elapsed().as_secs() > wallet_core::markets::DIRECTORY_TTL))
        {
            self.eco.markets_view.pools_loading = true;
            self.eco.markets_view.pools_attempted = Some(Instant::now());
            self.send_data(DataCmd::MarketPools);
            return;
        }
        self.unstick_markets();
        // Live reserves keep the rate, the pool's TVL and the chart's last price moving.
        self.tick_reserves();
        let Some((pool, _)) = self.swap_pool() else { return };
        // The chart reads the pool's own trades as Markets does, not only the indexer's candles:
        // the indexer lags, and it has no candles at all for a launch-AMM, QuaiSwap or Hartii pair.
        let _ = self.tick_pair(pool, SWAP_CHART_BUCKET);
    }

    pub(crate) fn swap_submit(&mut self) {
        if !self.can_sign() {
            self.toast("this wallet is watch-only", true);
            return;
        }
        let card = &self.eco.swap;
        let Some(to) = card.to.clone() else {
            self.toast("pick a token to receive (/)", true);
            return;
        };
        if card.from.decimals() == UNKNOWN_DECIMALS || to.decimals() == UNKNOWN_DECIMALS {
            self.info("still reading the token's decimals…");
            return;
        }
        if amount::parse_amount(&card.amount, card.from.decimals()).map_or(true, |a| a.is_zero()) {
            self.toast("enter an amount to pay", true);
            return;
        }
        // The token's curve pays more than the exchanges here (or they cannot fill it): trade on the
        // curve, through the same reviews Launches uses, proofs of its destination included.
        if self.swap_quote_current()
            && let Some(offer) = self.swap_uses_curve()
        {
            let account = self.dash.accounts.first().map(|a| a.address.clone());
            let (amount, slippage, deadline) = (card.amount.clone(), card.slippage_bps, Some(card.deadline_minutes));
            let (token, symbol, curve) = (offer.token.clone(), offer.symbol.clone(), offer.curve.clone());
            if offer.sell {
                self.start_flow(FlowKind::Steps {
                    prepare: Box::new(Prepare::CurveSellNext { account, token, symbol: symbol.clone(), curve, amount, slippage, deadline }),
                    label: format!("sell {symbol} to its curve"),
                });
            } else {
                self.send(Cmd::Prepare(Prepare::CurveBuy { account, token, symbol, curve, amount, slippage, deadline }));
            }
            return;
        }
        let quote = match (&card.quote, self.swap_quote_current()) {
            (Some(Ok(q)), true) => q.clone(),
            (Some(Err(e)), true) => {
                let e = e.clone();
                self.toast(e, true);
                return;
            }
            _ => {
                self.info("waiting for a fresh quote…");
                return;
            }
        };
        let from_id = match &card.from {
            SwapAsset::Quai => "quai".to_string(),
            SwapAsset::Token { address, .. } => address.clone(),
        };
        let to_id = match &to {
            SwapAsset::Quai => "quai".to_string(),
            SwapAsset::Token { address, .. } => address.clone(),
        };
        let network = self.net();
        let wquai = network.as_ref().and_then(|n| n.wquai.clone()).unwrap_or_default();
        let paying_wquai = !wquai.is_empty() && from_id.eq_ignore_ascii_case(&wquai);
        // Paying a WQUAI pool from QUAI: wrap what is missing first (each step still reviewed).
        let mut prewrap = None;
        if quote.insufficient && paying_wquai {
            let needed = amount::parse_amount(&card.amount, 18).unwrap_or(U256::ZERO);
            let missing = needed.saturating_sub(U256::from(self.wrapped_atoms(false)));
            // The swap is signed by the first account, so only its QUAI can be wrapped.
            let quai = self.dash.accounts.first().map_or(U256::ZERO, |a| a.balance);
            if quai > missing {
                prewrap = Some(amount::format_amount(missing, 18));
            }
        }
        if quote.insufficient && prewrap.is_none() {
            let wqi = network.as_ref().and_then(|n| n.wqi.clone()).is_some_and(|w| from_id.eq_ignore_ascii_case(&w));
            self.toast(
                format!(
                    "not enough {} to pay this amount{}",
                    card.from.symbol(),
                    if wqi { " · wrapped Qi must be claimed first (Trade › Wrap › Claim WQI)" } else { "" }
                ),
                true,
            );
            return;
        }
        let account = self.dash.accounts.first().map(|a| a.address.clone());
        // A swap that pays out WQUAI offers to redeem it for QUAI afterwards.
        let redeem = !wquai.is_empty() && to_id.eq_ignore_ascii_case(&wquai);
        // Across both exchanges: swap to the hub first; the second swap is sized once it confirms.
        let (first_to, then, unwrap_after) = match (quote.hub(), quote.legs.first()) {
            (Some((hub, _)), Some(first)) => {
                let hub_decimals = if first.output_decimals == 0 { 18 } else { first.output_decimals };
                (hub, Some(NextSwap { to: to_id.clone(), unwrap_after: redeem, hub_decimals, first: None, polls: 0 }), false)
            }
            _ => (to_id.clone(), None, redeem),
        };
        let label =
            format!("swap {} {} → {}{}", card.amount, card.from.symbol(), to.symbol(), if then.is_some() { " (two swaps)" } else { "" });
        let (amount, slippage, deadline) = (card.amount.clone(), card.slippage_bps, card.deadline_minutes);
        if prewrap.is_none() && !redeem {
            let Some(owner) = account.clone() else {
                self.toast("select a signing account", true);
                return;
            };
            let action = if then.is_some() {
                wallet_core::execution::TradingAction::CrossVenue {
                    from: from_id,
                    to: to_id,
                    hub: first_to,
                    amount,
                    stage: 0,
                    slippage,
                    deadline,
                }
            } else {
                wallet_core::execution::TradingAction::Swap { from: from_id, to: to_id, amount, slippage, deadline }
            };
            self.start_flow(FlowKind::Steps {
                prepare: Box::new(Prepare::Trading {
                    intent: wallet_core::execution::TradingIntent { account: owner, max_fee: None, action },
                }),
                label,
            });
            return;
        }
        if let Some(missing) = &prewrap {
            self.info(format!("wrapping {missing} QUAI first, then the swap"));
        }
        self.start_flow(FlowKind::Swap {
            account,
            from: from_id,
            to: first_to,
            amount,
            slippage,
            deadline,
            label,
            prewrap,
            unwrap_after,
            baseline: self.wrapped_atoms(false).to_string(),
            then,
        });
    }

    /// Turn a two-exchange route whose first swap confirmed into its second swap, sized from
    /// exactly what the first paid out (recorded from its receipt). False while that output is not
    /// visible yet.
    pub(crate) fn begin_second_swap(&self, flow: &mut Flow) -> bool {
        let FlowKind::Swap { from, to, amount, unwrap_after, then, .. } = &mut flow.kind else { return false };
        let Some(next) = then.clone() else { return false };
        let Some(first) = next.first.as_deref() else { return false };
        let paid = self
            .dash
            .ops
            .iter()
            .find(|o| o.id == first)
            .and_then(|o| o.detail["actual_out"].as_str())
            .and_then(|v| U256::from_str_radix(v, 10).ok())
            .filter(|v| !v.is_zero());
        let Some(paid) = paid else { return false };
        *amount = amount::format_amount(paid, next.hub_decimals);
        *from = std::mem::replace(to, next.to);
        *unwrap_after = next.unwrap_after;
        *then = None;
        flow.swapped = false;
        true
    }
}
