//! Markets: pair order, prices, charts, the DEX flow and alerts.

use super::*;

impl App {
    /// Alerts: read them once, and check them every minute while no daemon is running (the
    /// daemon checks them itself, and two checkers would each fire).
    pub(crate) fn tick_alerts(&mut self) {
        if self.locked || self.meta.is_none() {
            return;
        }
        if !self.eco.alerts_loaded {
            self.eco.alerts_loaded = true;
            self.send_data(DataCmd::Alerts(super::super::data::AlertOp::Load));
            return;
        }
        if self.eco.alerts.is_empty() || self.eco.alerts_checked.is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
            return;
        }
        self.eco.alerts_checked = Some(Instant::now());
        // Whether the daemon already covers them is read by the data worker, not here: opening
        // the database is not the UI thread's to do.
        self.send_data(DataCmd::Alerts(super::super::data::AlertOp::Check { pairs: self.config.features.trading, unless_daemon: true }));
    }

    /// The pool that trades the swap card's pair directly (the deepest, if several do), and
    /// whether the pay token is its token0. Drawn from the Markets list, so it is a display aid:
    /// the quote reads the router on-chain regardless.
    pub fn swap_pool(&self) -> Option<(wallet_core::markets::Pool, bool)> {
        let to = self.eco.swap.to.as_ref()?;
        let wquai = self.net().and_then(|n| n.wquai.clone()).map(|w| w.to_lowercase());
        let address = |a: &SwapAsset| match a {
            SwapAsset::Quai => wquai.clone(),
            SwapAsset::Token { address, .. } => Some(address.to_lowercase()),
        };
        let (pay, get) = (address(&self.eco.swap.from)?, address(to)?);
        let Some(Ok((pools, _))) = &self.eco.markets_view.pools else { return None };
        pools
            .iter()
            .filter(|p| p.venue != wallet_core::markets::Venue::Curve)
            .filter(|p| {
                let (a, b) = (p.token0.address.to_lowercase(), p.token1.address.to_lowercase());
                (a == pay && b == get) || (a == get && b == pay)
            })
            .max_by(|a, b| a.tvl_usd.unwrap_or(0.0).total_cmp(&b.tvl_usd.unwrap_or(0.0)))
            .map(|p| (p.clone(), p.token0.address.eq_ignore_ascii_case(&pay)))
    }

    /// Which pair the chart is showing: the cursor while the pairs list has it, else the pair
    /// the cursor left behind when it moved to the flow column.
    pub fn markets_pair(&self) -> usize {
        // Off Markets (the trader layout draws it beside the swap card) the cursor belongs to that
        // screen, so the chart keeps the pair it was left on.
        if self.screen != Screen::Markets || self.pane == 1 { self.eco.markets_view.pair_selected } else { self.selected }
    }

    /// The pairs list in the order it is shown. A pair with no figure to sort on goes last, so
    /// the rows carrying the number the user asked for are the ones at the top.
    /// The pairs as Markets lists them. The order is sorted once per change of the directory, the
    /// sort or the watchlist, not per call: this runs from every loop iteration and every frame,
    /// and used to clone and sort the whole directory each time.
    pub fn market_rows(&self) -> Vec<&wallet_core::markets::Pool> {
        let mv = &self.eco.markets_view;
        let Some(Ok((pools, _))) = &mv.pools else { return Vec::new() };
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (mv.sort as u8).hash(&mut h);
            self.eco.watchlist.hash(&mut h);
            for p in pools {
                (p.address.as_str(), p.tvl_usd.map(f64::to_bits), p.change_24h().map(f64::to_bits)).hash(&mut h);
            }
            h.finish()
        };
        if self.eco.market_order.borrow().as_ref().is_none_or(|(k, _)| *k != key) {
            *self.eco.market_order.borrow_mut() = Some((key, self.sort_markets_order(pools)));
        }
        let order = self.eco.market_order.borrow();
        order.as_ref().map(|(_, o)| o.iter().filter_map(|i| pools.get(*i)).collect()).unwrap_or_default()
    }

    pub(crate) fn sort_markets_order(&self, pools: &[wallet_core::markets::Pool]) -> Vec<usize> {
        let mv = &self.eco.markets_view;
        // A few dollars seeded beside a token's real market lists as a second pair nobody trades.
        // It stays in Pools and the router still sees it; the pairs list shows the market, unless
        // the pair is watched.
        let shadowed = wallet_core::markets::shadowed(pools);
        let watched = |i: &usize| self.eco.watchlist.iter().any(|w| w.eq_ignore_ascii_case(&pools[*i].address));
        let mut rows: Vec<usize> = (0..pools.len()).filter(|i| !shadowed.contains(i) || watched(i)).collect();
        let key = |i: &usize| match mv.sort {
            MarketSort::TvlDesc | MarketSort::TvlAsc => pools[*i].tvl_usd,
            MarketSort::ChangeDesc | MarketSort::ChangeAsc => pools[*i].change_24h(),
            MarketSort::Default => None,
        };
        match mv.sort {
            MarketSort::Default => {}
            MarketSort::TvlDesc | MarketSort::ChangeDesc => {
                rows.sort_by(|a, b| key(b).is_some().cmp(&key(a).is_some()).then(key(b).unwrap_or(0.0).total_cmp(&key(a).unwrap_or(0.0))));
            }
            MarketSort::TvlAsc | MarketSort::ChangeAsc => {
                rows.sort_by(|a, b| key(b).is_some().cmp(&key(a).is_some()).then(key(a).unwrap_or(0.0).total_cmp(&key(b).unwrap_or(0.0))));
            }
        }
        // Watched pairs stay at the top whatever the order: watching one is the user saying it
        // belongs in front. The sort still decides the order within each group (a stable sort).
        if !self.eco.watchlist.is_empty() {
            rows.sort_by_key(|i| !watched(i));
        }
        rows
    }

    /// The pool the chart is showing.
    pub fn selected_pool(&self) -> Option<wallet_core::markets::Pool> {
        let rows = self.market_rows();
        rows.get(self.markets_pair().min(rows.len().saturating_sub(1))).map(|p| (*p).clone())
    }

    /// A token's USD price: USDT is a dollar, wrapped QUAI takes the portfolio's QUAI price,
    /// and anything else is worth what the token market says, if it says anything.
    pub fn token_usd(&self, token: &wallet_core::markets::PoolToken) -> Option<f64> {
        // A network the config cannot name still has tokens the market prices: only the two
        // the profile identifies depend on it.
        if let Some(network) = self.net() {
            if network.ecosystem.usdt.as_ref().is_some_and(|u| u.address.eq_ignore_ascii_case(&token.address)) {
                return Some(1.0);
            }
            if network.wquai.as_ref().is_some_and(|w| w.eq_ignore_ascii_case(&token.address)) {
                return self.eco.portfolio.as_ref().and_then(|p| p.prices.as_ref()).and_then(|b| b.quai_usd);
            }
        }
        self.eco.markets.iter().find(|m| m.address.eq_ignore_ascii_case(&token.address)).and_then(|m| m.price_usd)
    }

    /// What a swap was worth, priced from whichever side has a price.
    pub fn swap_usd(&self, swap: &wallet_core::markets::DexSwap) -> Option<f64> {
        self.token_usd(&swap.token_in)
            .map(|p| p * swap.amount_in)
            .or_else(|| self.token_usd(&swap.token_out).map(|p| p * swap.amount_out))
            .filter(|v| v.is_finite())
    }

    /// The flow rows the column shows: every swap worth at least the threshold. A swap nobody
    /// can price is kept — hiding what cannot be judged would drop real trades silently.
    pub fn flow_rows(&self) -> Vec<&wallet_core::markets::DexSwap> {
        let min = self.eco.markets_view.flow_min_usd;
        self.eco.markets_view.flow.iter().filter(|s| min <= 0.0 || self.swap_usd(s).is_none_or(|v| v >= min)).collect()
    }

    /// The base of a tape row — the side the row is about, and the side a green or red arrow
    /// refers to.
    ///
    /// A pool the directory carries decides it exactly as the pairs list does. A curve trade has
    /// no pair to look up: it happened on the curve contract, and only a bonded HartiiLabs curve
    /// is listed as a pool at all. So the base falls back to whichever side is not the money —
    /// the launch token, which is what was bought or sold. Without this every curve row drew in
    /// plain text with no price, as though the wallet could not tell which way it went.
    pub fn flow_base<'a>(
        &self,
        swap: &'a wallet_core::markets::DexSwap,
        pool: Option<&'a wallet_core::markets::Pool>,
    ) -> Option<&'a wallet_core::markets::PoolToken> {
        if let Some(p) = pool {
            return Some(if self.pool_base0(p) { &p.token0 } else { &p.token1 });
        }
        let network = self.net()?;
        let money = |t: &wallet_core::markets::PoolToken| {
            network.wquai.as_deref().is_some_and(|w| w.eq_ignore_ascii_case(&t.address))
                || network.ecosystem.usdt.as_ref().is_some_and(|u| u.address.eq_ignore_ascii_case(&t.address))
        };
        // Both sides money, or neither: nothing here says which one the row is about.
        match (money(&swap.token_in), money(&swap.token_out)) {
            (true, false) => Some(&swap.token_out),
            (false, true) => Some(&swap.token_in),
            _ => None,
        }
    }

    /// Whether token0 is the base (priced in the quote), honoring the user's flip.
    pub fn pool_base0(&self, pool: &wallet_core::markets::Pool) -> bool {
        let network = self.net();
        let usdt = network.as_ref().and_then(|n| n.ecosystem.usdt.as_ref().map(|u| u.address.clone()));
        let natural = wallet_core::markets::base_is_token0(
            pool,
            usdt.as_deref(),
            network.as_ref().and_then(|n| n.wquai.as_deref()),
            network.as_ref().and_then(|n| n.wqi.as_deref()),
        );
        natural != self.eco.markets_view.flipped.contains(&pool.address)
    }

    /// Display symbol for a pool token (WQUAI trades as native QUAI through the router).
    pub fn market_symbol(&self, token: &wallet_core::markets::PoolToken) -> String {
        let wquai = self.net().and_then(|n| n.wquai.clone()).map(|w| w.to_lowercase());
        if wquai.as_deref() == Some(token.address.as_str()) { "QUAI".into() } else { token.symbol.clone() }
    }

    /// Candles for the chart: the indexer's when it has this pool and timeframe, else the ones
    /// built from the pool's own logs. Both are display data, and they agree within a percent.
    pub fn chart_candles(
        &self,
        pool: &wallet_core::markets::Pool,
        base0: bool,
        bucket: u64,
        count: usize,
    ) -> Arc<Vec<wallet_core::markets::Candle>> {
        self.chart_candles_at(pool, base0, bucket, count, wallet_core::registry::now())
    }

    pub(crate) fn chart_candles_at(
        &self,
        pool: &wallet_core::markets::Pool,
        base0: bool,
        bucket: u64,
        count: usize,
        now: u64,
    ) -> Arc<Vec<wallet_core::markets::Candle>> {
        let key = self.eco.markets_view.derived_key(pool, base0, bucket, count, now);
        if let Some(found) = self.eco.markets_view.derived.borrow().candles.get(&key) {
            return found.clone();
        }
        let value = Arc::new(self.build_chart_candles(pool, base0, bucket, count, now));
        let mut cache = self.eco.markets_view.derived.borrow_mut();
        if cache.candles.len() >= 32 {
            cache.candles.clear();
        }
        cache.candles.insert(key, value.clone());
        value
    }

    pub fn market_stats(&self, pool: &wallet_core::markets::Pool, base0: bool, now: u64) -> wallet_core::markets::PairStats {
        let key = self.eco.markets_view.derived_key(pool, base0, 3600, 24, now);
        if let Some(found) = self.eco.markets_view.derived.borrow().stats.get(&key) {
            return found.clone();
        }
        let events = self.eco.markets_view.events.get(&pool.address).and_then(|v| v.as_ref().ok()).map_or(&[][..], Vec::as_slice);
        let value = wallet_core::markets::pair_stats(events, pool, base0, now);
        let mut cache = self.eco.markets_view.derived.borrow_mut();
        if cache.stats.len() >= 128 {
            cache.stats.clear();
        }
        cache.stats.insert(key, value.clone());
        value
    }

    pub fn market_trades(&self, pool: &wallet_core::markets::Pool, base0: bool) -> Arc<Vec<wallet_core::markets::Trade>> {
        let key = self.eco.markets_view.derived_key(pool, base0, 0, 0, 0);
        if let Some(found) = self.eco.markets_view.derived.borrow().trades.get(&key) {
            return found.clone();
        }
        let events = self.eco.markets_view.events.get(&pool.address).and_then(|v| v.as_ref().ok()).map_or(&[][..], Vec::as_slice);
        let value = Arc::new(wallet_core::markets::trades(events, pool, base0));
        let mut cache = self.eco.markets_view.derived.borrow_mut();
        if cache.trades.len() >= 8 {
            cache.trades.clear();
        }
        cache.trades.insert(key, value.clone());
        value
    }

    pub(crate) fn build_chart_candles(
        &self,
        pool: &wallet_core::markets::Pool,
        base0: bool,
        bucket: u64,
        count: usize,
        now: u64,
    ) -> Vec<wallet_core::markets::Candle> {
        let events = match self.eco.markets_view.events.get(&pool.address) {
            Some(Ok(ev)) => ev.as_slice(),
            _ => &[],
        };
        let local = wallet_core::markets::candles_in_zone(events, pool, base0, bucket, 0, now, count);
        let Some(indexed) = self.eco.markets_view.candles.get(&(pool.address.clone(), bucket)).filter(|v| !v.is_empty()) else {
            return local;
        };
        let fix = |v: f64| if !base0 && v > 0.0 { 1.0 / v } else { v };
        let mut merged: std::collections::BTreeMap<_, _> = indexed
            .iter()
            .map(|c| {
                (
                    c.start,
                    wallet_core::markets::Candle {
                        start: c.start,
                        open: fix(c.open),
                        close: fix(c.close),
                        high: if !base0 { fix(c.low) } else { c.high },
                        low: if !base0 { fix(c.high) } else { c.low },
                        volume: c.volume,
                        trades: c.trades,
                    },
                )
            })
            .collect();
        // A full local bucket replaces the indexed version, avoiding duplicate volume.
        // For a partial bucket retain indexed OHLC and update its last observed price only;
        // covered history, rather than receipt time, decides what can replace the indexer.
        let first_event = events.iter().map(|e| e.position().0).filter(|at| *at > 0).min();
        for c in local {
            if first_event.is_some_and(|at| at <= c.start) || !merged.contains_key(&c.start) {
                merged.insert(c.start, c);
            } else if let Some(held) = merged.get_mut(&c.start) {
                held.close = c.close;
                held.high = held.high.max(c.high);
                held.low = held.low.min(c.low);
            }
        }
        merged.into_values().rev().take(count).collect::<Vec<_>>().into_iter().rev().collect()
    }

    /// The DEX-wide tape, from one `quai_getLogs` over every pool. The first pass reads a window
    /// of blocks; later ones only what the chain added, so the request stays small.
    /// Live reserves for the pools on screen.
    ///
    /// This is the one market read that is not behind somebody else's cache: the explorer serves
    /// its pool page `max-age=30`, so no client-side tuning gets price or TVL under half a minute.
    /// The node has no such floor, and one multicall covers the whole directory.
    pub(crate) fn tick_reserves(&mut self) {
        let Some(Ok((pools, _))) = self.eco.markets_view.pools.as_ref().map(|r| r.as_ref()) else { return };
        if pools.is_empty() {
            return;
        }
        let mv = &self.eco.markets_view;
        if mv.reserves_loading || mv.reserves_attempted.is_some_and(|t| t.elapsed() < self.feed_pace()) {
            return;
        }
        let pools = pools.clone();
        self.eco.markets_view.reserves_loading = true;
        self.eco.markets_view.reserves_attempted = Some(Instant::now());
        self.send_data(DataCmd::PoolReserves { pools });
    }

    pub(crate) fn tick_dex_flow(&mut self) {
        use wallet_core::markets::FLOW_BLOCKS;
        let Some(Ok((pools, _))) = &self.eco.markets_view.pools else { return };
        if pools.is_empty() {
            return;
        }
        let mv = &self.eco.markets_view;
        if mv.flow_loading || mv.flow_at.is_some_and(|t| t.elapsed() < self.feed_pace()) {
            return;
        }
        let pools = pools.clone();
        self.eco.markets_view.flow_loading = true;
        self.eco.markets_view.flow_asked = Some(Instant::now());
        self.send_data(DataCmd::DexFlow { pools, blocks: FLOW_BLOCKS });
    }

    pub(crate) fn markets_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            // Watch the pair: it moves to the top and stays there.
            KeyCode::Char('w') if self.pane == 0 => {
                if let Some(pool) = self.selected_pool() {
                    let name = self.pair_name(&pool);
                    self.send_data(DataCmd::Alerts(super::super::data::AlertOp::ToggleWatch { pool: pool.address, name }));
                }
                true
            }
            // Set an alert on the pair, starting from its price now.
            KeyCode::Char('A') if self.pane == 0 => {
                if let Some(pool) = self.selected_pool() {
                    let base0 = self.pool_base0(&pool);
                    let name = self.pair_name(&pool);
                    let now = pool.spot_price().map(|p| if base0 { p } else { 1.0 / p });
                    self.open_form(super::super::app::FormKind::Alert { pool: pool.address.clone(), name, inverted: !base0 });
                    if let (super::super::app::Modal::Form(f), Some(p)) = (&mut self.modal, now) {
                        f.fields[1].value = format!("{p:.6}").trim_end_matches('0').trim_end_matches('.').to_string();
                    }
                }
                true
            }
            KeyCode::Char('T') => {
                let mv = &mut self.eco.markets_view;
                mv.timeframe = (mv.timeframe + 1) % wallet_core::markets::TIMEFRAMES.len();
                let label = wallet_core::markets::TIMEFRAMES[mv.timeframe].0;
                self.info(format!("chart: {label} candles"));
                true
            }
            // Order by depth, then by how far the pair moved today.
            KeyCode::Char('L') => self.sort_markets(MarketSort::next_tvl),
            KeyCode::Char('M') => self.sort_markets(MarketSort::next_change),
            KeyCode::Char('f') => {
                if let Some(pool) = self.selected_pool() {
                    let flipped = &mut self.eco.markets_view.flipped;
                    if !flipped.remove(&pool.address) {
                        flipped.insert(pool.address);
                    }
                }
                true
            }
            KeyCode::Char('R') => {
                self.eco.markets_view.pools_at = None;
                self.eco.markets_view.events_at.clear();
                self.eco.markets_view.flow_at = None;
                self.tick_markets();
                true
            }
            // The cursor belongs to one pane at a time; each keeps its place while the other has it.
            KeyCode::Tab | KeyCode::BackTab => {
                let mv = &mut self.eco.markets_view;
                if self.pane == 0 {
                    mv.pair_selected = self.selected;
                } else {
                    mv.flow_selected = self.selected;
                }
                self.pane = 1 - self.pane;
                self.selected = if self.pane == 0 {
                    self.eco.markets_view.pair_selected
                } else {
                    self.eco.markets_view.flow_selected.min(self.flow_rows().len().saturating_sub(1))
                };
                true
            }
            // Dust is most of a busy tape: step the floor up until the trades that matter show.
            KeyCode::Char('m') => {
                let mv = &mut self.eco.markets_view;
                mv.flow_min_usd = match mv.flow_min_usd {
                    v if v < 1.0 => 1.0,
                    v if v < 10.0 => 10.0,
                    v if v < 100.0 => 100.0,
                    _ => 0.0,
                };
                let floor = mv.flow_min_usd;
                if self.pane == 1 {
                    self.selected = self.selected.min(self.flow_rows().len().saturating_sub(1));
                }
                self.toast(
                    if floor <= 0.0 { "flow: every swap".to_string() } else { format!("flow: swaps over {}", amount::usd(floor)) },
                    false,
                );
                true
            }
            KeyCode::Char('t') | KeyCode::Enter if self.pane == 1 => {
                // A swap in the flow names its pair: take the chart there.
                let Some(pool) = self.flow_rows().get(self.selected).map(|s| s.pool.clone()) else { return true };
                if let Some(Ok((pools, _))) = &self.eco.markets_view.pools {
                    match pools.iter().position(|p| p.address == pool) {
                        Some(i) => {
                            self.eco.markets_view.pair_selected = i;
                            let name = pools.get(i).map(|p| self.pair_name(p)).unwrap_or_default();
                            self.info(format!("chart: {name}"));
                        }
                        None => self.toast("that pool is not in the directory", true),
                    }
                }
                true
            }
            KeyCode::Char('t') | KeyCode::Enter => {
                self.trade_selected_pool();
                true
            }
            // A curve takes both sides; `t` buys on it, `S` sells to it.
            KeyCode::Char('S') if self.pane == 0 => {
                self.sell_to_selected_curve();
                true
            }
            _ => false,
        }
    }

    /// Open the sale form for the selected pair when it is a bonding curve, prefilled with what
    /// the wallet holds. A pool is sold through the swap card, which `t` opens and `f` flips.
    pub(crate) fn sell_to_selected_curve(&mut self) {
        let Some(pool) = self.selected_pool() else { return };
        if pool.venue != wallet_core::markets::Venue::Curve {
            self.info("S sells to a bonding curve; for a pool, t opens the swap card and f flips it to sell");
            return;
        }
        if !self.can_sign() {
            self.toast("this wallet is watch-only", true);
            return;
        }
        let token = pool.token0.address.clone();
        let held = self
            .eco
            .portfolio
            .as_ref()
            .and_then(|p| p.rows.iter().find(|r| matches!(&r.key, AssetKey::Token(a) if a.eq_ignore_ascii_case(&token))))
            .map(|r| amount::format_amount(r.amount(), r.decimals))
            .unwrap_or_default();
        self.open_form(FormKind::CurveSell { token, symbol: pool.token0.symbol.clone(), curve: pool.address.clone(), held });
    }

    /// A pair as the lists name it, base first.
    pub fn pair_name(&self, pool: &wallet_core::markets::Pool) -> String {
        let (base, quote) = if self.pool_base0(pool) { (&pool.token0, &pool.token1) } else { (&pool.token1, &pool.token0) };
        format!("{}/{}", self.market_symbol(base), self.market_symbol(quote))
    }

    /// Open the swap card to buy the pair's base with its quote.
    pub(crate) fn trade_selected_pool(&mut self) {
        let Some(pool) = self.selected_pool() else { return };
        // A token on its bonding curve is bought on the curve, not through a router.
        if pool.venue == wallet_core::markets::Venue::Curve {
            if !self.can_sign() {
                self.toast("this wallet is watch-only", true);
                return;
            }
            let (token, symbol) = (pool.token0.address.clone(), pool.token0.symbol.clone());
            self.open_form(FormKind::CurveBuy { token, symbol, curve: pool.address.clone() });
            return;
        }
        let base0 = self.pool_base0(&pool);
        let (base, quote) = if base0 { (&pool.token0, &pool.token1) } else { (&pool.token1, &pool.token0) };
        let wquai = self.net().and_then(|n| n.wquai.clone()).map(|w| w.to_lowercase());
        let asset = |t: &wallet_core::markets::PoolToken| {
            if wquai.as_deref() == Some(t.address.as_str()) {
                SwapAsset::Quai
            } else {
                SwapAsset::Token { address: t.address.clone(), symbol: t.symbol.clone(), decimals: t.decimals }
            }
        };
        self.eco.swap.from = asset(quote);
        self.eco.swap.to = Some(asset(base));
        self.eco.swap.amount.clear();
        self.eco.swap.quote = None;
        self.eco.swap.field = 1;
        let text = format!("buy {} with {} · f flips to sell", self.market_symbol(base), self.market_symbol(quote));
        self.eco.markets_view.pair_selected = self.markets_pair();
        self.switch(Screen::Swap);
        self.toast(text, false);
    }

    /// Icon URL for a token contract, from the portfolio or market data; `quai` and `qi` get the
    /// bundled logos.
    pub fn asset_icon_url(&self, contract: &str) -> Option<String> {
        if let Some(native) = wallet_core::media::native_icon(contract) {
            return Some(native.to_string());
        }
        let rows = self.eco.portfolio.as_ref().map(|p| p.rows.as_slice()).unwrap_or_default();
        let from_rows = rows
            .iter()
            .find(|r| match &r.key {
                AssetKey::Quai => contract.eq_ignore_ascii_case("quai"),
                AssetKey::Token(a) => a.eq_ignore_ascii_case(contract),
                _ => false,
            })
            .and_then(|r| r.icon_url.clone());
        from_rows
            .or_else(|| self.eco.markets.iter().find(|m| m.address.eq_ignore_ascii_case(contract)).and_then(|m| m.icon_url.clone()))
            // Launch-zone tokens are too new for the explorer's icon set; Quainance has their logos.
            .or_else(|| self.eco.launch_logos.get(&contract.to_lowercase()).cloned())
    }

    /// Icon URL for a portfolio row (bundled logos for QUAI and Qi).
    pub fn row_icon(&self, r: &wallet_core::portfolio::AssetRow) -> Option<String> {
        match &r.key {
            AssetKey::Token(_) => r.icon_url.clone(),
            key => wallet_core::media::native_icon(&key.id()).map(str::to_string),
        }
    }

    /// Icon contract for a pool token: wrapped QUAI is shown as QUAI.
    pub fn pool_icon_contract(&self, token: &wallet_core::markets::PoolToken) -> String {
        let wquai = self.net().and_then(|n| n.wquai.clone());
        if wquai.is_some_and(|w| w.eq_ignore_ascii_case(&token.address)) { "quai".into() } else { token.address.clone() }
    }

    /// Re-order the pairs list and keep the cursor on the pair it was on.
    pub(crate) fn sort_markets(&mut self, next: impl Fn(MarketSort) -> MarketSort) -> bool {
        let holding = self.selected_pool().map(|p| p.address);
        self.eco.markets_view.sort = next(self.eco.markets_view.sort);
        let label = self.eco.markets_view.sort.label();
        if let Some(address) = holding
            && let Some(i) = self.market_rows().iter().position(|p| p.address == address)
        {
            // Only the pairs list owns `selected`; while the flow column has the cursor, moving it
            // here would drag that column's cursor to a row number that means nothing in it.
            if self.screen == Screen::Markets && self.pane == 0 {
                self.selected = i;
            }
            self.eco.markets_view.pair_selected = i;
        }
        self.info(format!("pairs by {label}"));
        true
    }

    /// Refresh the pool directory, the DEX-wide tape, live reserves and the selected pool's own
    /// trades. A tick that falls inside a source's cache TTL is served from the store and never
    /// reaches the network, so this paces the screen rather than the network.
    pub(crate) fn tick_markets(&mut self) {
        self.unstick_markets();
        let mv = &self.eco.markets_view;
        let stale_pools = mv.pools_at.is_none_or(|t| t.elapsed().as_secs() > wallet_core::markets::DIRECTORY_TTL);
        if !mv.pools_loading && (mv.pools.is_none() || stale_pools) && mv.pools_attempted.is_none_or(|at| at.elapsed() >= MARKET_REFRESH) {
            self.eco.markets_view.pools_loading = true;
            self.eco.markets_view.pools_attempted = Some(Instant::now());
            self.send_data(DataCmd::MarketPools);
            return;
        }
        self.tick_dex_flow();
        self.tick_reserves();
        let Some(pool) = self.selected_pool() else { return };
        // Scrolling the list is not a request for every row it passes over. A row is only asked
        // about once the cursor has rested on it, which is what turns a 26-row scroll from one
        // fetch per row into one fetch for the row the user stopped at.
        let settled = match &self.eco.markets_view.selected_at {
            Some((address, at)) if *address == pool.address => at.elapsed() >= SELECTION_SETTLES,
            _ => {
                self.eco.markets_view.selected_at = Some((pool.address.clone(), Instant::now()));
                false
            }
        };
        if !settled {
            return;
        }
        let bucket = wallet_core::markets::TIMEFRAMES[self.eco.markets_view.timeframe].1;
        let since = self.tick_pair(pool, bucket);
        if let Some(since) = since {
            self.prefetch_neighbours(bucket, since);
        }
    }

    /// Keep one pair's chart fed: the indexer's candles for `bucket` and the pool's own trades,
    /// each at the screen's pace. Returns the history window when the pair already has its
    /// trades and nothing of its own is in flight, which is when neighbours may load.
    pub(crate) fn tick_pair(&mut self, pool: wallet_core::markets::Pool, bucket: u64) -> Option<u64> {
        let since = wallet_core::registry::now()
            .saturating_sub(bucket * (MARKET_CANDLES as u64 + 1))
            .max(wallet_core::registry::now().saturating_sub(30 * 86_400));
        let mv = &self.eco.markets_view;
        let due = match mv.events_at.get(&pool.address) {
            None => true,
            Some((at, window)) => at.elapsed() > self.feed_pace() || *window > since,
        };
        // The indexer already has this timeframe bucketed, so ask for it alongside the logs: the
        // chart can draw from whichever lands first, and the logs are still needed for the tape.
        let want_candles = wallet_core::subgraph::interval_for(bucket).is_some()
            && mv.candles_requested.get(&(pool.address.clone(), bucket)).is_none_or(|at| at.elapsed() >= Duration::from_secs(5));
        let events_idle = mv.events_loading.is_none();
        if want_candles {
            self.eco.markets_view.candles_requested.insert((pool.address.clone(), bucket), Instant::now());
            self.send_data(DataCmd::PairCandles { pool: pool.address.clone(), bucket, count: MARKET_CANDLES });
        }
        if due && events_idle {
            self.eco.markets_view.events_loading = Some(pool.address.clone());
            self.eco.markets_view.events_at.insert(pool.address.clone(), (Instant::now(), since));
            self.send_data(DataCmd::PoolEvents { pool: Box::new(pool), since });
            return None;
        }
        matches!(self.eco.markets_view.events.get(&pool.address), Some(Ok(_))).then_some(since)
    }

    /// Clear a read that has been in flight far longer than any source takes.
    ///
    /// Each market feed asks again only once its last request has answered. An answer can go
    /// missing: the data worker drops what it finished for a configuration it has since left
    /// (a settings change, a new monitoring node), and a source can hang. Without this, one lost
    /// answer froze that feed — prices, TVL, the tape or a chart — until the wallet restarted.
    pub(crate) fn unstick_markets(&mut self) {
        let mv = &mut self.eco.markets_view;
        let stuck = |at: Option<Instant>| at.is_some_and(|at| at.elapsed() > MARKET_STUCK);
        if mv.pools_loading && stuck(mv.pools_attempted) {
            mv.pools_loading = false;
        }
        if mv.reserves_loading && stuck(mv.reserves_attempted) {
            mv.reserves_loading = false;
        }
        if mv.flow_loading && stuck(mv.flow_asked) {
            mv.flow_loading = false;
        }
        for slot in [&mut mv.events_loading, &mut mv.events_prefetching] {
            if slot.as_ref().is_some_and(|pool| stuck(mv.events_at.get(pool).map(|(at, _)| *at))) {
                *slot = None;
            }
        }
    }

    /// Load the chart of a pair near the cursor before the cursor reaches it, so moving down the
    /// list lands on a drawn chart instead of a spinner. Only pairs never loaded, one at a time, and
    /// only once the selected pair has its own: the node caps concurrent log reads, and the pair on
    /// screen comes first.
    fn prefetch_neighbours(&mut self, bucket: u64, since: u64) {
        let mv = &self.eco.markets_view;
        if mv.events_loading.is_some() || mv.events_prefetching.is_some() {
            return;
        }
        let at = self.markets_pair() as isize;
        let rows = self.market_rows();
        let next = super::PREFETCH_ROWS
            .iter()
            .filter_map(|d| usize::try_from(at + d).ok())
            .filter_map(|i| rows.get(i))
            .find(|p| !mv.events_at.contains_key(&p.address))
            .map(|p| (*p).clone());
        drop(rows);
        let Some(pool) = next else { return };
        let want_candles = wallet_core::subgraph::interval_for(bucket).is_some()
            && !self.eco.markets_view.candles_requested.contains_key(&(pool.address.clone(), bucket));
        if want_candles {
            self.eco.markets_view.candles_requested.insert((pool.address.clone(), bucket), Instant::now());
            self.send_data(DataCmd::PairCandles { pool: pool.address.clone(), bucket, count: MARKET_CANDLES });
        }
        self.eco.markets_view.events_prefetching = Some(pool.address.clone());
        self.eco.markets_view.events_at.insert(pool.address.clone(), (Instant::now(), since));
        self.send_data(DataCmd::PoolEvents { pool: Box::new(pool), since });
    }
}
