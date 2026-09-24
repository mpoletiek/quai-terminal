//! What the data worker sends back, and asking it for more: portfolio, preloads, images.

use super::*;

impl App {
    pub fn send_data(&self, cmd: DataCmd) {
        // A feature turned off reads nothing for it, whichever path asked.
        if cmd.feature().is_some_and(|f| !self.config.features.on(f)) {
            return;
        }
        if let Some(d) = &self.data {
            let _ = d.tx.send(cmd);
        }
    }

    /// Start the data worker once a wallet is known.
    pub fn start_data_worker(&mut self) {
        if self.data.is_some() {
            return;
        }
        let Some(meta) = &self.meta else { return };
        let Some(network) = self.net() else { return };
        let path = self.paths.wallet_dir(&meta.id).join("app.sqlite");
        let shared = self.paths.shared_cache();
        if let Ok(worker) = super::super::data::DataWorker::spawn(path, shared, (*network).clone(), self.config.data_policy()) {
            self.data = Some(worker);
            self.on_view_opened();
            self.preload();
        }
    }

    /// Rebind the data worker after a network switch or a data-source change.
    pub fn data_policy_changed(&mut self) {
        if let Some(network) = self.net() {
            self.send_data(DataCmd::Configure { network: (*network).clone(), policy: self.config.data_policy(), app_db: None });
        }
        self.eco.portfolio_requested = None;
        self.eco.portfolio_signature = None;
        self.eco.images.retain(|_, s| matches!(s, ImageSlot::Ready(..)));
        self.maybe_refresh_portfolio(true);
    }

    /// Clear network-bound ecosystem data (after a network switch).
    pub fn reset_eco_for_network(&mut self) {
        let swap_prefs = (self.eco.swap.slippage_bps, self.eco.swap.deadline_minutes);
        let images = std::mem::take(&mut self.eco.images);
        self.eco = super::super::eco::Eco::default();
        self.eco.images = images;
        (self.eco.swap.slippage_bps, self.eco.swap.deadline_minutes) = swap_prefs;
        self.detail.clear();
        self.data_policy_changed();
    }

    /// Open another wallet: the session follows, the keys of the old one are dropped, and every
    /// cached view belongs to the wallet that is leaving, so all of it goes. The new wallet's
    /// cache database is its own, so the data worker is rebound to it.
    pub fn switch_wallet(&mut self, id: &str) {
        let Ok(meta) = self.registry.resolve(Some(id), None) else {
            return self.toast(format!("no wallet `{id}`"), true);
        };
        if self.meta.as_ref().is_some_and(|m| m.id == meta.id) {
            return self.info(format!("already on `{}`", meta.name));
        }
        let name = meta.name.clone();
        self.config.default_wallet = Some(name.clone());
        self.save_config();
        self.busy = Some(format!("opening {name}…"));
        self.send(Cmd::SwitchWallet(meta.id.clone()));
        // Images are keyed by URL, not by wallet, but everything else is this wallet's.
        self.eco = super::super::eco::Eco::default();
        // The screen stays where it is, so nothing re-opens it to ask for the new wallet's data.
        // The dashboard that brings the new accounts does it instead.
        self.reload_view_on_accounts = true;
        self.detail.clear();
        self.selected = 0;
        self.dash = super::super::worker::Dashboard {
            meta: Some(meta.clone()),
            network_id: self.network_id.clone(),
            network_name: self.dash.network_name.clone(),
            networks: std::mem::take(&mut self.dash.networks),
            ..super::super::worker::Dashboard::default()
        };
        self.meta = Some(meta.clone());
        // The new wallet's lock screen shows now, not when the worker reaches the switch: it may
        // be in a sync step that cannot stop, and the password can be checked meanwhile.
        if meta.kind != wallet_core::registry::WalletKind::Watch {
            self.enter_lock(None);
            self.switch_lock_pending = true;
        }
        if let Some(network) = self.net() {
            let app_db = self.paths.wallet_dir(&meta.id).join("app.sqlite");
            self.send_data(DataCmd::Configure { network: (*network).clone(), policy: self.config.data_policy(), app_db: Some(app_db) });
        }
    }

    /// Exact balances the portfolio starts from, taken from the wallet worker's dashboard.
    pub fn known_from_dash(&self) -> Known {
        let owners: Vec<String> = self.dash.accounts.iter().map(|a| a.address.clone()).collect();
        let quai = self.dash.accounts.iter().fold(U256::ZERO, |s, a| s.saturating_add(a.balance));
        let mut tokens: std::collections::BTreeMap<String, (String, String, u8, U256)> = std::collections::BTreeMap::new();
        for t in &self.dash.tokens {
            let e = tokens.entry(t.token.address.to_lowercase()).or_insert((
                t.token.symbol.clone(),
                t.token.name.clone(),
                t.token.decimals,
                U256::ZERO,
            ));
            e.3 = e.3.saturating_add(t.balance);
        }
        Known {
            owners,
            quai,
            qi: self.dash.qi.as_ref().map(|q| q.balance.total),
            tokens: tokens.into_iter().map(|(a, (s, n, d, b))| (a, s, n, d, b)).collect(),
        }
    }

    /// Ask for a new portfolio when balances changed or it is older than a minute.
    pub fn maybe_refresh_portfolio(&mut self, force: bool) {
        if self.dash.accounts.is_empty() || self.data.is_none() {
            return;
        }
        let known = self.known_from_dash();
        let signature =
            format!("{}:{}:{:?}:{:?}", self.dash.network_id, known.quai, known.qi, known.tokens.iter().map(|t| t.4).collect::<Vec<_>>());
        let stale = self.eco.portfolio_requested.is_none_or(|t| t.elapsed() > Duration::from_secs(60));
        if force || stale || self.eco.portfolio_signature.as_deref() != Some(signature.as_str()) {
            self.eco.portfolio_requested = Some(Instant::now());
            self.eco.portfolio_signature = Some(signature);
            self.send_data(DataCmd::Portfolio(known));
        }
    }

    /// Load what a view needs when it opens.
    pub fn on_view_opened(&mut self) {
        // What this screen waits on goes to the front of the data worker's queue.
        self.send_data(DataCmd::Focus(focus_jobs(self.screen)));
        match self.screen {
            Screen::Home => self.maybe_refresh_portfolio(false),
            Screen::Collected if self.eco.nfts.is_none() && !self.eco.nfts_loading => {
                self.load_nfts(false);
                self.load_my_listings();
            }
            Screen::Markets => {
                if self.eco.markets.is_empty() {
                    self.send_data(DataCmd::Markets);
                }
                self.tick_markets();
            }
            Screen::Board => self.tick_board(),
            Screen::Launches => self.load_launches(false),
            Screen::Pnl => self.load_pnl(false),
            Screen::Orders => self.orders_list(),
            Screen::Network => self.tick_chain_stats(),
            Screen::Wallets => self.load_wallets(),
            Screen::Explore => {
                if self.eco.collections.is_none() && !self.eco.collections_loading {
                    self.eco.collections_loading = true;
                    self.send_data(DataCmd::Collections { query: None });
                }
                self.load_nft_market(false);
            }
            Screen::Listings => {
                if !self.eco.listings.contains_key(&None) && !self.eco.listings_loading {
                    self.eco.listings_loading = true;
                    self.send_data(DataCmd::Listings { collection: None });
                }
                self.load_nft_market(false);
            }
            Screen::Swap => {
                if self.eco.markets.is_empty() {
                    self.send_data(DataCmd::Markets);
                }
                self.maybe_refresh_portfolio(false);
                if self.eco.swap.to.is_none() {
                    self.eco.swap.to = self.default_receive_asset();
                }
            }
            // Settled wrapped Qi waiting for its claim: open the card on Claim WQI.
            Screen::Wrap
                if self.eco.wrap.amount.is_empty()
                    && self
                        .dash
                        .wrap
                        .as_ref()
                        .and_then(|w| w.unclaimed_qits.as_deref())
                        .is_some_and(|q| q.parse::<u128>().is_ok_and(|v| v > 0)) =>
            {
                self.eco.wrap.mode = 1;
            }
            Screen::Accounts if self.eco.lockups.is_none() && !self.dash.accounts.is_empty() => {
                self.send_data(DataCmd::Lockups(self.dash.accounts.iter().map(|a| a.address.clone()).collect()));
            }
            _ => {}
        }
    }

    /// Warm every section once the wallet's accounts are known, so screens open on loaded data.
    /// The data worker answers from its cache first and paces third-party requests.
    pub fn preload(&mut self) {
        let owners = self.owner_addresses();
        if self.eco.preloaded || owners.is_empty() || self.data.is_none() {
            return;
        }
        self.eco.preloaded = true;
        self.maybe_refresh_portfolio(false);
        // Only the features that are on: their loading flags would otherwise wait for an answer
        // that never comes.
        let features = self.config.features;
        if features.nfts {
            if self.eco.nfts.is_none() && !self.eco.nfts_loading {
                self.load_nfts(false);
            }
            if self.eco.my_listings.is_none() {
                self.load_my_listings();
            }
            if self.eco.collections.is_none() && !self.eco.collections_loading {
                self.eco.collections_loading = true;
                self.send_data(DataCmd::Collections { query: None });
            }
            if !self.eco.listings.contains_key(&None) && !self.eco.listings_loading {
                self.eco.listings_loading = true;
                self.send_data(DataCmd::Listings { collection: None });
            }
        }
        if features.trading {
            if self.eco.markets.is_empty() {
                self.send_data(DataCmd::Markets);
            }
            let mv = &mut self.eco.markets_view;
            if mv.pools.is_none() && !mv.pools_loading {
                mv.pools_loading = true;
                mv.pools_at = Some(Instant::now());
                self.send_data(DataCmd::MarketPools);
            }
        }
        // MAX on native QUAI cannot be honest without this, and the picker's route badges want
        // the pools, so both load before the user opens Trade rather than when they do.
        if self.eco.gas_price.is_none() {
            self.send_data(DataCmd::GasPrice);
        }
        if self.eco.lockups.is_none() {
            self.send_data(DataCmd::Lockups(owners));
        }
        // The launch zone brings its tokens' logos, which Markets uses for graduated and on-curve
        // tokens too: loaded now, both screens open with their pictures.
        self.load_launches(false);
    }

    /// The wallet's Quai addresses: from the dashboard once it has loaded, else from the wallet
    /// file (known before any network call).
    pub fn owner_addresses(&self) -> Vec<String> {
        if !self.dash.accounts.is_empty() {
            return self.dash.accounts.iter().map(|a| a.address.clone()).collect();
        }
        self.meta.as_ref().map(|m| m.quai_owner_addresses()).unwrap_or_default()
    }

    /// Fetch images a screen is likely to show before it is opened (loaded from the wallet's
    /// image cache when seen before). Queued behind what is on screen now.
    pub(crate) fn preload_images(&mut self, wants: impl IntoIterator<Item = (Option<String>, u32)>) {
        use wallet_core::media::{ICON, is_native_icon};
        let mut batch = Vec::new();
        for (url, edge) in wants {
            let Some(url) = url else { continue };
            let allowed =
                if edge > ICON && !is_native_icon(&url) { self.config.images } else { self.config.token_icons || is_native_icon(&url) };
            // IPFS gateways are slow and strictly paced: those load when shown, not ahead.
            let gateway = wallet_core::ipfs::is_gateway_url(&url);
            let key = (url, edge);
            if allowed && !gateway && !batch.contains(&key) && !self.eco.images.contains_key(&key) {
                self.eco.images.insert(key.clone(), ImageSlot::Loading);
                batch.push(key);
            }
        }
        if !batch.is_empty() {
            // Reversed: the image lane takes the newest want first, so the first listed loads first.
            batch.reverse();
            self.send_data(DataCmd::Images(batch));
        }
    }

    /// Handle a data worker event.
    pub fn on_data_event(&mut self, ev: DataEv) {
        self.dirty = true;
        // News for the panel in front of you glints its border once: a quote landing where it
        // was asked for, the chart of the pair you picked, a total that actually moved.
        let news = match &ev {
            DataEv::SwapQuote { .. } => self.screen == Screen::Swap,
            DataEv::ProtocolQuote { .. } => self.screen == Screen::Convert,
            DataEv::LiquidityQuote { .. } => self.screen == Screen::Pools,
            DataEv::PairCandles { .. } => self.screen == Screen::Markets,
            DataEv::Portfolio(Ok(p)) => {
                self.screen == Screen::Home && self.eco.portfolio.as_ref().is_some_and(|old| (old.total_usd - p.total_usd).abs() >= 0.01)
            }
            _ => false,
        };
        if news {
            self.glint_at = Some(Instant::now());
        }
        match ev {
            DataEv::LpPositions { result, gauge, zone } => {
                let pv = &mut self.eco.pools_view;
                pv.loading = false;
                pv.loaded_at = Some(Instant::now());
                if let Some(g) = gauge {
                    pv.gauge = Some(*g);
                }
                if let Some(z) = zone {
                    pv.zone = Some(*z);
                }
                // A failed refresh keeps the last good list rather than blanking the screen.
                if result.is_ok() || pv.positions.as_ref().is_none_or(|p| p.is_err()) {
                    pv.positions = Some(result);
                }
                let len = pv.positions.as_ref().and_then(|r| r.as_ref().ok()).map_or(0, Vec::len);
                pv.selected = pv.selected.min(len.saturating_sub(1));
            }
            DataEv::PairCandles { pool, bucket, candles } => {
                if !candles.is_empty() {
                    // How long a chart took to draw from a standing start: measured from the
                    // moment the cursor settled on this pool, which is when it was asked for.
                    if !self.eco.markets_view.candles.contains_key(&(pool.clone(), bucket))
                        && let Some((settled, at)) = &self.eco.markets_view.selected_at
                        && *settled == pool
                    {
                        wallet_core::diag::timing("chart.cold", *at);
                    }
                    if self.eco.markets_view.candles.get(&(pool.clone(), bucket)) != Some(&candles) {
                        self.eco.markets_view.changed(&pool);
                        self.eco.markets_view.candles.insert((pool, bucket), candles);
                    }
                }
            }
            DataEv::Launches(result) => {
                self.eco.launches_at = Some(Instant::now());
                // Keep the last good list when a refresh fails.
                if result.is_ok() || !matches!(self.eco.launches, Some(Ok(_))) {
                    self.eco.launches = Some(result);
                }
            }
            DataEv::LaunchLogos(logos) => self.eco.launch_logos.extend(logos),
            DataEv::WalletQuai(totals) => self.wallet_quai.extend(totals),
            DataEv::Alerts { alerts, watchlist, fired, note } => {
                self.eco.alerts = alerts;
                let reorder = self.eco.watchlist != watchlist;
                // The pair under the cursor, read before the watchlist moves it.
                let holding = reorder.then(|| self.selected_pool().map(|p| p.address)).flatten();
                self.eco.watchlist = watchlist;
                self.eco.alerts_loaded = true;
                if reorder {
                    self.keep_cursor_on(holding);
                }
                if let Some(note) = note {
                    self.toast(note, false);
                }
                for (title, body) in fired {
                    self.toast_as(format!("{title} · {body}"), super::super::app::Severity::Attention, None);
                    self.send(super::super::worker::Cmd::Refresh { full: false });
                }
            }
            DataEv::ChainStats(result) => {
                // Keep the last good figures when a refresh fails.
                if result.is_ok() || !matches!(self.eco.chain_stats, Some(Ok(_))) {
                    self.eco.chain_stats = Some(result.map(|b| *b));
                }
            }
            DataEv::CurveMarket { token, result } => {
                if result.is_ok() || !matches!(self.eco.curves.get(&token), Some(Ok(_))) {
                    self.eco.curves.insert(token, result.map(|b| *b));
                }
            }
            DataEv::TxCost { hash, result } => {
                self.eco.tx_costs.insert(hash, result);
            }
            DataEv::GasPrice(r) => {
                if let Ok(price) = r.and_then(|p| U256::from_str_radix(&p, 10).map_err(|e| e.to_string())) {
                    self.eco.gas_price = Some(price);
                }
            }
            DataEv::Portfolio(Ok(p)) => {
                let first = self.eco.portfolio.is_none();
                wallet_core::diag::mark("startup.portfolio");
                let has_nfts = p.nfts.items > 0;
                use wallet_core::media::{ICON, ICON_LARGE};
                let icons: Vec<(Option<String>, u32)> = p
                    .rows
                    .iter()
                    .flat_map(|r| {
                        let url = self.row_icon(r);
                        [(url.clone(), ICON), (url, ICON_LARGE)]
                    })
                    .collect();
                // Leave this wallet's summary for the cockpit, which shows every wallet at once.
                if let Some(m) = &self.meta {
                    wallet_core::cockpit::save_summary(&self.paths, &m.id, &p);
                    self.wallet_summaries
                        .insert(m.id.clone(), wallet_core::cockpit::load_summary(&self.paths, &m.id, &p.network).unwrap_or_default());
                }
                self.eco.portfolio = Some(*p);
                self.eco.portfolio_error = None;
                self.preload_images(icons);
                // Home shows a few NFT thumbnails when the wallet holds any and images are on.
                if has_nfts && self.screen == Screen::Home && self.config.images && self.eco.nfts.is_none() && !self.eco.nfts_loading {
                    self.load_nfts(false);
                }
                if first && !self.config.data_disclosure_shown && self.config.explorer_lookups && self.dash.network_id == "mainnet" {
                    self.config.data_disclosure_shown = true;
                    self.save_config();
                    self.toast(
                        "Portfolio data comes from explorer.qu.ai, which can see your addresses and IP. System › Data sources to turn off.",
                        false,
                    );
                }
            }
            DataEv::Portfolio(Err(e)) => self.eco.portfolio_error = Some(e),
            DataEv::Notice(text) => self.toast(text, true),
            DataEv::MarketPools(mut r) => {
                self.eco.markets_view.pools_loading = false;
                // The same USD basis the live reserves use (below), or each refresh would flip the
                // TVL column between the directory's price and the feed's.
                if let Ok((pools, _)) = r.as_mut() {
                    let wquai = self.net().and_then(|n| n.wquai.clone());
                    let usd = self.eco.portfolio.as_ref().and_then(|p| p.prices.as_ref()).and_then(|b| b.quai_usd);
                    if usd.is_some() {
                        wallet_core::markets::reprice_tvl(pools, wquai.as_deref(), usd);
                    }
                }
                if r.is_ok() {
                    self.eco.markets_view.pools_at = Some(Instant::now());
                }
                // Keep showing the last good directory when a refresh fails.
                if r.is_ok() || self.eco.markets_view.pools.as_ref().is_none_or(|p| p.is_err()) {
                    let holding = self.selected_pool().map(|p| p.address);
                    self.eco.markets_view.pools = Some(r);
                    self.keep_cursor_on(holding);
                }
            }
            DataEv::PoolReserves(result) => {
                self.eco.markets_view.reserves_loading = false;
                if result.as_ref().is_ok_and(|fresh| !fresh.is_empty()) {
                    self.eco.markets_view.reserves_at = Some(Instant::now());
                }
                // Silent on failure: the directory's own numbers are still on screen, only a few
                // seconds older. A node that cannot answer must not paint an error over a working
                // market list.
                if let Ok(fresh) = result
                    && !fresh.is_empty()
                    && let Some(Ok((pools, _))) = self.eco.markets_view.pools.as_mut().map(|r| r.as_mut())
                {
                    let network = self.config.network(&self.network_id).ok();
                    let wquai = network.as_ref().and_then(|n| n.wquai.clone());
                    // The price feed, not the on-chain USDT pool: that pool holds about four
                    // thousand dollars, and pricing the whole exchange off its spot put every
                    // computed TVL 1.8% under the explorer's. The feed is what the explorer uses.
                    let usd = self.eco.portfolio.as_ref().and_then(|p| p.prices.as_ref()).and_then(|b| b.quai_usd);
                    wallet_core::markets::apply_reserves(pools, &fresh, wquai.as_deref(), usd);
                    self.dirty = true;
                }
            }
            DataEv::DexFlow(result) => {
                self.eco.markets_view.flow_loading = false;
                self.eco.markets_view.flow_at = Some(Instant::now());
                match result {
                    // The tape keeps what it has when a refresh fails; the column says so.
                    Ok(flow) => {
                        self.eco.markets_view.flow_error = None;
                        if !flow.is_empty() || self.eco.markets_view.flow.is_empty() {
                            self.eco.markets_view.flow = flow;
                        }
                    }
                    Err(e) => self.eco.markets_view.flow_error = Some(e),
                }
            }
            DataEv::Board { channel, result } => {
                if self.eco.board.loading.as_deref() == Some(channel.as_str()) {
                    self.eco.board.loading = None;
                }
                self.eco.board.at.insert(channel.clone(), Instant::now());
                // Reading a channel is what makes it read.
                if self.screen == Screen::Board && self.board_channel().as_deref() == Some(channel.as_str()) {
                    self.mark_board_seen(&channel);
                    self.eco.board.announced.remove(&channel);
                }
                // Keep the messages already read when a refresh fails.
                if result.is_ok() || !matches!(self.eco.board.posts.get(&channel), Some(Ok(_))) {
                    self.eco.board.posts.insert(channel, result);
                }
            }
            DataEv::BoardChannels(result) => {
                self.eco.board.known_loading = false;
                self.eco.board.known_at = Some(Instant::now());
                // Keep what was found when a scan fails; an empty board is not news.
                if let Ok(found) = result {
                    let first = self.eco.board.seen.is_empty();
                    self.eco.board.known = found;
                    self.announce_board(first);
                }
            }
            DataEv::PoolEvents { pool, coverage, result } => {
                if let Some(coverage) = coverage {
                    self.eco.markets_view.history_coverage.insert(pool.clone(), coverage);
                } else {
                    self.eco.markets_view.history_coverage.remove(&pool);
                }
                if self.eco.markets_view.events_loading.as_deref() == Some(pool.as_str()) {
                    self.eco.markets_view.events_loading = None;
                }
                if self.eco.markets_view.events_prefetching.as_deref() == Some(pool.as_str()) {
                    self.eco.markets_view.events_prefetching = None;
                }
                if (result.is_ok() || !matches!(self.eco.markets_view.events.get(&pool), Some(Ok(_))))
                    && self.eco.markets_view.events.get(&pool) != Some(&result)
                {
                    self.eco.markets_view.changed(&pool);
                    self.eco.markets_view.events.insert(pool, result);
                }
            }
            DataEv::Image { url, edge, rendition, transient } => {
                let slot = match rendition {
                    Some(r) => {
                        ImageSlot::Ready(r, if self.motion().effects() { Instant::now() } else { Instant::now() - Duration::from_secs(1) })
                    }
                    None => ImageSlot::Failed(Instant::now() + if transient { IMAGE_RETRY_SOON } else { IMAGE_RETRY }),
                };
                self.eco.images.insert((url, edge), slot);
            }
            DataEv::QiRoutes { key, result } => {
                if key == self.eco.convert.requested_key {
                    self.eco.convert.routes_key = key;
                    self.eco.convert.routes = Some(result.map(|c| *c));
                }
            }
            DataEv::ProtocolQuote { key, card, result } => {
                if key == self.eco.convert.protocol_key.get() {
                    match result {
                        Ok(quote) if card && self.screen == Screen::Convert => {
                            self.on_event(super::super::worker::Ev::Quote(quote), (0, 0))
                        }
                        Ok(quote) if !card => self.modal = Modal::Quote(quote),
                        Ok(_) => {}
                        Err(error) => self.toast(error, true),
                    }
                }
            }
            DataEv::Markets(m) => {
                self.preload_images(m.iter().take(40).map(|t| (t.icon_url.clone(), wallet_core::media::ICON)));
                self.eco.markets = m;
                if self.eco.swap.to.is_none() {
                    self.eco.swap.to = self.default_receive_asset();
                }
            }
            DataEv::SwapQuote { key, result } => {
                if key != 0 && key == self.eco.swap.requested_key && self.swap_input_key() == self.eco.swap.requested_input {
                    let approval_done = self.eco.swap.approving && matches!(&result, Ok(q) if !q.approval_needed);
                    self.eco.swap.quote = Some(result.map(|b| *b));
                    self.eco.swap.quote_key = key;
                    self.eco.swap.quoted_at = Some(Instant::now());
                    if approval_done && self.eco.flow.is_none() {
                        self.eco.swap.approving = false;
                    }
                }
            }
            DataEv::LiquidityQuote { key, result } => {
                if let Some(card) = self.eco.pools_view.add.as_mut()
                    && key == card.requested_key
                {
                    card.quote = Some(result.map(|b| *b));
                    card.quote_key = key;
                }
            }
            DataEv::Nfts(r) => {
                if let Ok(v) = &r {
                    self.preload_images(v.iter().map(|n| (n.item.image.clone(), wallet_core::media::THUMB)));
                }
                self.eco.nfts_loading = false;
                self.eco.nfts = Some(r);
            }
            DataEv::Collections { result } => {
                if let Ok(v) = &result {
                    self.preload_images(v.iter().take(30).map(|c| (c.preview.clone(), wallet_core::media::THUMB)));
                }
                self.eco.collections_loading = false;
                self.eco.collections = Some(result);
            }
            DataEv::CollectionItems { contract, offset, result } => self.collection_page(contract, offset, result),
            DataEv::CollectionStats { result } => match result {
                Ok(rows) => {
                    self.eco.nft_stats = rows.into_iter().map(|c| (c.address.clone(), c)).collect();
                    self.eco.nft_stats_error = None;
                }
                Err(e) => self.eco.nft_stats_error = Some(e),
            },
            DataEv::NftTrades { result } => {
                if let Ok(rows) = result {
                    self.eco.nft_trades = rows;
                }
            }
            DataEv::Listings { collection, result } => {
                // The indexer's images are usually raw IPFS files; the explorer's metadata points
                // at its resized media proxy, so look that up for the first rows.
                if let Ok(v) = &result {
                    for l in v.iter().take(20) {
                        self.eco.want_meta(&l.contract, &l.token_id);
                    }
                    self.flush_meta_wants();
                }
                if collection.is_none() {
                    self.eco.listings_loading = false;
                }
                self.eco.listings.insert(collection, result);
            }
            DataEv::MyListings(result) => self.eco.my_listings = Some(result),
            DataEv::Nft { contract, token_id, result } => {
                if let Ok(item) = &result {
                    self.preload_images([(item.image.clone(), wallet_core::media::THUMB)]);
                }
                self.eco.nft_meta.insert((contract.to_lowercase(), token_id), result.map(|b| *b));
            }
            DataEv::Ask { contract, token_id, result } => {
                self.eco.asks.insert((contract, token_id), result.map(|b| *b));
            }
            DataEv::TokenInfo { address, result } => {
                if let Ok((info, _)) = &result
                    && let Some(d) = info.decimals
                {
                    let card = &mut self.eco.swap;
                    for asset in [Some(&mut card.from), card.to.as_mut()].into_iter().flatten() {
                        if let SwapAsset::Token { address: a, decimals, .. } = asset
                            && *a == address
                            && *decimals == UNKNOWN_DECIMALS
                        {
                            *decimals = d;
                            card.edited = Some(Instant::now());
                        }
                    }
                }
                self.eco.token_info.insert(address, result);
            }
            DataEv::Lockups(r) => self.eco.lockups = Some(r),
            DataEv::Test(results) => {
                self.eco.testing = false;
                let failed = results.iter().filter(|(_, r, _)| r.is_err()).count();
                self.toast(
                    if failed == 0 {
                        "all data sources answered".to_string()
                    } else {
                        format!("{} did not answer", wallet_core::amount::count(failed, "data source"))
                    },
                    failed > 0,
                );
                self.eco.test = Some(results);
            }
        }
    }

    /// Send newly wanted images to the data worker (called after each frame).
    pub fn flush_image_wants(&mut self) {
        let wants: Vec<(String, u32)> = self.eco.wants.borrow_mut().drain(..).collect();
        let mut batch = Vec::new();
        for (url, edge) in wants {
            let key = (url.clone(), edge);
            let due = match self.eco.images.get(&key) {
                None => true,
                Some(ImageSlot::Failed(retry)) => Instant::now() >= *retry,
                Some(_) => false,
            };
            if due {
                self.eco.images.insert(key, ImageSlot::Loading);
                batch.push((url, edge));
            }
        }
        if !batch.is_empty() {
            self.send_data(DataCmd::Images(batch));
        }
        self.flush_meta_wants();
    }

    /// Request NFT metadata wanted since the last flush. Marked loading by inserting nothing:
    /// the data worker's single-flight keeps repeats from reaching the explorer.
    ///
    /// Every caller of `want_meta` is a marketplace listings row, so these are public: the same
    /// rows every wallet loads, cached once for the whole data directory.
    pub fn flush_meta_wants(&mut self) {
        let wants: Vec<(String, String)> = self.eco.meta_wants.borrow_mut().drain(..).collect();
        for (contract, token_id) in wants {
            if self.eco.meta_requested.insert((contract.clone(), token_id.clone())) {
                self.send_data(DataCmd::Nft { contract, token_id, public: true });
            }
        }
    }

    /// Ask for network statistics when the Network screen has none or they are five minutes old
    /// (the feed's own freshness window; asking sooner would only read the cache).
    pub fn tick_chain_stats(&mut self) {
        if self.eco.chain_stats_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(300)) {
            self.eco.chain_stats_at = Some(Instant::now());
            self.send_data(DataCmd::ChainStats);
        }
    }

    /// A new block: everything on screen that reads chain state is due now.
    ///
    /// Each feed keeps its own clock, which paces it between blocks and keeps a failing source
    /// from being hammered. A block is the event those clocks approximate, so it expires them:
    /// reserves (prices, TVL), the DEX tape, the selected pair's trades, LP positions, the curve
    /// being looked at and the board, then asks at once. A feed still in flight is left to land;
    /// the block after next catches anything it missed.
    pub fn on_block(&mut self, height: u64) {
        if height <= self.eco.head {
            return;
        }
        self.eco.head = height;
        let due = Instant::now().checked_sub(MARKET_STUCK);
        let mv = &mut self.eco.markets_view;
        mv.reserves_attempted = None;
        mv.flow_at = None;
        for (at, _) in mv.events_at.values_mut() {
            if let Some(due) = due {
                *at = due;
            }
        }
        self.eco.pools_view.loaded_at = None;
        self.eco.curves_at.clear();
        self.eco.board.at.clear();
        self.eco.board.dm_at.clear();
        if !self.locked {
            self.tick_eco();
        }
    }

    /// Periodic ecosystem work: debounced swap quotes and re-quotes while waiting on approval.
    pub fn tick_eco(&mut self) {
        self.advance_flow();
        self.poll_handoff();
        self.tick_chat();
        self.tick_alerts();
        self.tick_tx_cost();
        self.page_collection();
        self.tick_orders();
        if self.screen == Screen::Markets && !self.locked {
            self.tick_markets();
        }
        if self.screen == Screen::Board && !self.locked {
            self.tick_board();
        }
        self.tick_board_watch();
        if self.screen == Screen::Convert && !self.locked {
            self.tick_qi_routes();
        }
        if self.screen == Screen::Swap && !self.locked {
            self.tick_swap();
        }
        // Side by side, each half keeps its own data coming.
        if self.trader && !self.locked {
            match self.screen {
                Screen::Markets => self.tick_swap(),
                Screen::Swap => self.tick_markets(),
                _ => {}
            }
        }
        if self.screen == Screen::Network && !self.locked {
            self.tick_chain_stats();
        }
        // PnL is re-read on its own freshness window while it is on screen, not only when opened.
        if self.screen == Screen::Pnl && !self.locked {
            self.load_pnl(false);
        }
        if self.screen == Screen::Launches && !self.locked {
            self.load_launches(false);
            self.tick_curve();
        }
        if self.screen == Screen::Pools && !self.locked {
            self.tick_pools();
            self.tick_add_card();
        }
        // Home lists positions among the holdings, so it keeps them fresh too.
        if self.screen == Screen::Home && !self.locked && self.config.features.on(wallet_core::config::Feature::Trading) {
            self.tick_pools();
        }
        if self.screen != Screen::Swap || self.locked {
            return;
        }
        let card = &self.eco.swap;
        let Some(to) = card.to.clone() else { return };
        if card.from.decimals() == UNKNOWN_DECIMALS || to.decimals() == UNKNOWN_DECIMALS {
            return;
        }
        let decimals = card.from.decimals();
        let Ok(atoms) = amount::parse_amount(&card.amount, decimals) else { return };
        if atoms.is_zero() {
            return;
        }
        let Some(input) = self.swap_input_key() else { return };
        let debounced = card.edited.is_none_or(|t| t.elapsed() > Duration::from_millis(450));
        let refresh = card.quoted_at.is_some_and(|t| t.elapsed() > Duration::from_secs(if card.approving { 6 } else { 20 }));
        if debounced && (card.requested_key == 0 || Some(input) != card.requested_input || refresh) {
            let owner = self.dash.accounts.first().map(|a| a.address.clone());
            let from = card.from.clone();
            let slippage = card.slippage_bps;
            let key = self.eco.swap.request_sequence.wrapping_add(1).max(1);
            self.eco.swap.request_sequence = key;
            self.eco.swap.requested_key = key;
            self.eco.swap.requested_input = Some(input);
            self.eco.swap.quoted_at = Some(Instant::now());
            self.send_data(DataCmd::SwapQuote { key, from, to, amount: atoms.to_string(), slippage, owner });
        }
    }
}
