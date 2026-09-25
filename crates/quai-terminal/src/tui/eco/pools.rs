//! Pools: liquidity positions, gauges and the add-liquidity card.

use super::*;

impl App {
    /// Load LP positions when Pools opens, and refresh them slowly afterwards. A position only
    /// moves when the user acts or the pool's reserves shift, so this is not a hot poll.
    pub(crate) fn tick_pools(&mut self) {
        if !self.eco.pools_view.positions.due(fresh::LP_POSITIONS, &self.eco.clock) {
            return;
        }
        let Some(Ok((pools, _))) = self.eco.markets_view.pools.shown() else {
            // Pools drive position discovery; `preload` has already asked for them.
            return;
        };
        // Both exchanges that hold real pairs. Filtering to the main one hid every launch-AMM pool
        // from position discovery, so a graduated token's LP could be neither seen nor staked even
        // when a launch-zone gauge was paying rewards on it — CHEEZ/QUAI being the case that found
        // this. A curve holds no LP at all, so it stays out.
        let pools: Vec<_> = pools.iter().filter(|p| p.venue.routable()).cloned().collect();
        let owners = self.owner_addresses();
        if owners.is_empty() {
            return;
        }
        self.eco.pools_view.positions.begin(&self.eco.clock);
        self.send_data(DataCmd::LpPositions { owners, pools });
    }

    /// Open the deposit card for a pool, sized by whichever side the user types.
    pub(crate) fn open_add_card(
        &mut self,
        pair: String,
        name: String,
        tokens: (wallet_core::markets::PoolToken, wallet_core::markets::PoolToken),
    ) {
        self.eco.pools_view.add = Some(AddCard {
            pair,
            name,
            token0: tokens.0,
            token1: tokens.1,
            side1: false,
            amount: String::new(),
            slippage_bps: self.config.swap_slippage_bps,
            account: self.dash.active_account().map(|a| a.address.clone()),
            field: 1,
            quote: None,
            quote_key: 0,
            requested_key: 0,
            edited: None,
        });
    }

    /// Price the open deposit card once typing settles, so the other side keeps up without a
    /// request per keystroke.
    pub(crate) fn tick_add_card(&mut self) {
        let Some(card) = self.eco.pools_view.add.as_ref() else { return };
        let Ok(atoms) = amount::parse_amount(&card.amount, card.typed().decimals) else { return };
        if atoms.is_zero() {
            return;
        }
        let key = hash_key(&[&card.pair, &card.typed().symbol, &atoms.to_string(), &card.slippage_bps.to_string()]);
        let debounced = card.edited.is_none_or(|t| t.elapsed() > Duration::from_millis(350));
        if !debounced || key == card.requested_key {
            return;
        }
        let (pair, token, slippage) = (card.pair.clone(), card.typed().symbol.clone(), card.slippage_bps);
        let (amount, owner) = (card.amount.clone(), card.account.clone());
        if let Some(card) = self.eco.pools_view.add.as_mut() {
            card.requested_key = key;
        }
        self.send_data(DataCmd::LiquidityQuote { key, pair, amount, token, slippage, owner });
    }

    /// Deposit-card keys. Either amount row can be the one you type: the other is the pool's
    /// answer, so the pair stays balanced whichever token you happen to have.
    pub(crate) fn add_card_key(&mut self, key: KeyEvent) -> bool {
        let accounts: Vec<String> = self.dash.accounts.iter().map(|a| a.address.clone()).collect();
        let Some(card) = self.eco.pools_view.add.as_mut() else { return false };
        // Fields: 0 account, 1 token0 amount, 2 token1 amount, 3 slippage.
        match key.code {
            KeyCode::Esc => {
                self.eco.pools_view.add = None;
                return true;
            }
            KeyCode::Tab | KeyCode::Down => card.field = (card.field + 1) % 4,
            KeyCode::BackTab | KeyCode::Up => card.field = (card.field + 3) % 4,
            KeyCode::Left | KeyCode::Right if card.field == 0 && !accounts.is_empty() => {
                let at = accounts.iter().position(|a| Some(a) == card.account.as_ref()).unwrap_or(0);
                let step = if key.code == KeyCode::Left { accounts.len() - 1 } else { 1 };
                card.account = Some(accounts[(at + step) % accounts.len()].clone());
            }
            KeyCode::Char('m') if card.field == 1 || card.field == 2 => {
                let side1 = card.field == 2;
                self.fill_add_max(side1);
                return true;
            }
            KeyCode::Enter => {
                self.submit_add_card();
                return true;
            }
            KeyCode::Backspace if card.field == 1 || card.field == 2 => {
                // Typing into a row makes it the side that is typed; the other becomes the
                // pool's answer, so a half-edited pair is never sent anywhere.
                card.side1 = card.field == 2;
                card.amount.pop();
                card.edited = Some(Instant::now());
                card.quote = None;
            }
            KeyCode::Backspace if card.field == 3 => {
                let mut text = card.slippage_bps.to_string();
                text.pop();
                card.slippage_bps = text.parse().unwrap_or(0);
            }
            KeyCode::Char(c) if (card.field == 1 || card.field == 2) && (c.is_ascii_digit() || c == '.') => {
                if card.side1 != (card.field == 2) {
                    card.side1 = card.field == 2;
                    card.amount.clear();
                    card.quote = None;
                }
                let decimals = card.typed().decimals;
                let mut text = std::mem::take(&mut card.amount);
                if digits_input(&mut text, &key, decimals) {
                    card.amount = text;
                    card.edited = Some(Instant::now());
                } else {
                    card.amount = text;
                }
            }
            KeyCode::Char(c) if card.field == 3 && c.is_ascii_digit() => {
                let text = format!("{}{c}", card.slippage_bps);
                if let Ok(bps) = text.parse::<u16>().map(|b| b.min(10_000)) {
                    card.slippage_bps = bps;
                }
            }
            _ => return false,
        }
        true
    }

    /// `m` on a deposit row: everything of that token the wallet can actually spend.
    pub(crate) fn fill_add_max(&mut self, side1: bool) {
        use wallet_core::spendable::token_max;
        let Some(card) = self.eco.pools_view.add.as_ref() else { return };
        let token = if side1 { card.token1.clone() } else { card.token0.clone() };
        let asset = SwapAsset::Token { address: token.address.clone(), symbol: token.symbol.clone(), decimals: token.decimals };
        let Some((balance, decimals)) = self.exact_balance(&asset) else {
            self.toast(format!("no exact {} balance yet — it is still loading", token.symbol), true);
            return;
        };
        if balance.is_zero() {
            self.toast(format!("no {} to deposit", token.symbol), true);
            return;
        }
        let m = token_max(balance, decimals);
        let (text, note) = (m.text(), m.note());
        if let Some(card) = self.eco.pools_view.add.as_mut() {
            card.side1 = side1;
            card.field = if side1 { 2 } else { 1 };
            card.amount = text;
            card.edited = Some(Instant::now());
            card.quote = None;
        }
        if let Some(note) = note {
            self.toast(note, false);
        }
    }

    /// Send the composed deposit to the review, the same three-step path the CLI takes.
    pub(crate) fn submit_add_card(&mut self) {
        let Some(card) = self.eco.pools_view.add.as_ref() else { return };
        if amount::parse_amount(&card.amount, card.typed().decimals).is_ok_and(|a| a.is_zero()) || card.amount.is_empty() {
            self.toast("enter an amount for one of the two sides", true);
            return;
        }
        if let Some(Err(e)) = card.quote.as_ref() {
            self.toast(wallet_core::session::short_code(e), true);
            return;
        }
        // Three reviews in a row — one exact approval per side, then the deposit — so it goes
        // through the sequence driver: each step is asked for again once the last one confirms.
        let prepare = Prepare::AddLiquidityNext {
            account: card.account.clone(),
            pair: card.pair.clone(),
            amount: card.amount.clone(),
            token: Some(card.typed().symbol.clone()),
            slippage: card.slippage_bps,
            deadline: self.config.swap_deadline_minutes,
        };
        let label = format!("add liquidity to {}", card.name);
        self.eco.pools_view.add = None;
        self.start_steps(prepare, label);
    }

    /// Trade › Pools keys: move between positions, then act on the focused one.
    pub(crate) fn pools_key(&mut self, key: KeyEvent) -> bool {
        // A deposit being composed takes the keys: it is a form, not a list.
        if self.eco.pools_view.add.is_some() {
            return self.add_card_key(key);
        }
        // Pane 0 is what you hold; pane 1 is every pool, which is where a new position starts.
        let len = if self.nav.pane == 0 { self.position_rows().len() } else { self.directory_rows().len() };
        let cursor = if self.nav.pane == 0 { &mut self.eco.pools_view.selected } else { &mut self.eco.pools_view.pool_selected };
        match key.code {
            KeyCode::Char('j') | KeyCode::Down if len > 0 => *cursor = (*cursor + 1).min(len - 1),
            KeyCode::Char('k') | KeyCode::Up => *cursor = cursor.saturating_sub(1),
            KeyCode::Char('R') => {
                self.eco.pools_view.positions.invalidate();
                self.tick_pools();
            }
            KeyCode::Char('a') => self.pool_action('a'),
            KeyCode::Char('r') => self.pool_action('r'),
            KeyCode::Char('s') => self.pool_action('s'),
            KeyCode::Char('u') => self.pool_action('u'),
            KeyCode::Char('h') => self.pool_action('h'),
            KeyCode::Char('e') => self.pool_action('e'),
            KeyCode::Char('i') => self.pool_action('i'),
            _ => return false,
        }
        true
    }

    /// Act on the focused position. Every path ends in a review, so nothing here moves value.
    pub(crate) fn pool_action(&mut self, action: char) {
        if !self.can_sign() {
            self.toast("this wallet is watch-only", true);
            return;
        }
        let Some(focus) = self.focused_pool() else {
            self.toast("no pool selected", true);
            return;
        };
        let (pair, name, tokens) = (focus.pair.clone(), focus.name.clone(), focus.tokens.clone());
        // Adding liquidity is the one action that does not need an existing position — it is how
        // a position starts. Everything else acts on something you already hold.
        if action == 'a' {
            self.open_add_card(pair, name, tokens);
            return;
        }
        let Some(position) = focus.position else {
            self.toast(format!("you have no liquidity in {name} yet — press a to add some"), true);
            return;
        };
        let account = self.dash.active_account().map(|a| a.address.clone());
        let lp = |v: U256| amount::format_amount(v, 18);
        let staked = position.pid.is_some();
        let gauge = position.gauge_address.clone();
        match action {
            'r' => self.open_form(FormKind::RemoveLiquidity { pair, name }),
            's' if !staked => self.toast(GAUGE_ABSENT, true),
            's' if position.lp_wallet.is_zero() => self.toast("no unstaked LP in the wallet", true),
            's' => {
                let amount = lp(position.lp_wallet);
                self.open_form(FormKind::StakePosition { pair, gauge, name, amount, stake: true });
            }
            'u' if position.lp_staked.is_zero() => self.toast("nothing staked in this pool", true),
            'u' => {
                let amount = lp(position.lp_staked);
                self.open_form(FormKind::StakePosition { pair, gauge, name, amount, stake: false });
            }
            'i' if !staked => self.toast(GAUGE_ABSENT, true),
            'i' if position.gauge == Some(wallet_core::gauge::GaugeKind::Zone) => self.toast(ZONE_NOT_FUNDABLE, true),
            'i' => self.open_form(FormKind::Incentivize { pair, name }),
            'h' | 'e' if !staked => self.toast(GAUGE_ABSENT, true),
            'h' => self.send(Cmd::Prepare(Prepare::Harvest { account, pair, gauge, exit: false })),
            'e' => self.send(Cmd::Prepare(Prepare::Harvest { account, pair, gauge, exit: true })),
            _ => {}
        }
    }

    /// Positions this wallet holds, newest read.
    /// The positions Home lists among the holdings: LP held or staked, with trading on.
    pub fn home_positions(&self) -> Vec<&wallet_core::liquidity::LpPosition> {
        if !self.config.features.on(wallet_core::config::Feature::Trading) {
            return Vec::new();
        }
        self.position_rows().iter().filter(|p| !p.lp_wallet.saturating_add(p.lp_staked).is_zero()).collect()
    }

    /// What the positions on Home are worth, where their pools are priced.
    pub fn pools_usd(&self) -> f64 {
        self.home_positions().iter().filter_map(|p| p.usd).sum()
    }

    pub fn position_rows(&self) -> &[wallet_core::liquidity::LpPosition] {
        self.eco.pools_view.positions.value().map_or(&[], Vec::as_slice)
    }

    /// Every main-exchange pool, deepest first — the directory a new position is opened from.
    /// Liquidity is added through the main router, so the launch AMM's pairs and the curves the
    /// Markets view also lists are not offered here.
    pub fn directory_rows(&self) -> Vec<wallet_core::markets::Pool> {
        match self.eco.markets_view.pools.shown() {
            // Both exchanges that hold LP. A launch-AMM pair is a real pool with real reserves and,
            // often, a launch-zone gauge paying rewards on it; leaving it out of the directory is
            // what made CHEEZ/QUAI impossible to stake from this screen. A curve has no LP token.
            Some(Ok((pools, _))) => pools.iter().filter(|p| p.venue.routable()).cloned().collect(),
            _ => Vec::new(),
        }
    }

    /// What the Pools screen is acting on: a pair, and the position in it when there is one.
    pub fn focused_pool(&self) -> Option<PoolFocus> {
        if self.nav.pane == 0 {
            let p = self.position_rows().get(self.eco.pools_view.selected)?;
            return Some(PoolFocus {
                pair: p.pair.clone(),
                name: p.name(),
                tokens: (p.token0.clone(), p.token1.clone()),
                position: Some(p.clone()),
            });
        }
        let pool = self.directory_rows().into_iter().nth(self.eco.pools_view.pool_selected)?;
        Some(PoolFocus {
            pair: pool.address.clone(),
            name: format!("{}/{}", pool.token0.symbol, pool.token1.symbol),
            tokens: (pool.token0.clone(), pool.token1.clone()),
            // A pool in the directory may also be one we hold; carry that so the actions work.
            position: self.position_rows().iter().find(|p| p.pair.eq_ignore_ascii_case(&pool.address)).cloned(),
        })
    }

    /// The gauge pool behind a pair, when it has one.
    pub fn gauge_pool_for(&self, pair: &str) -> Option<&wallet_core::gauge::GaugePool> {
        self.eco.pools_view.gauge.as_ref()?.pool_for(pair)
    }

    /// The launch-zone campaign on a pair, when one exists.
    pub fn zone_pool_for(&self, pair: &str) -> Option<&wallet_core::zone::ZonePool> {
        self.eco.pools_view.zone.as_ref()?.pool_for(pair)
    }

    /// A launch-zone campaign's APR, priced the same way the core gauge's is.
    pub fn zone_apr(&self, pool: &wallet_core::zone::ZonePool, tvl_usd: Option<f64>) -> Option<f64> {
        pool.apr(wallet_core::registry::now(), &|t| self.token_usd(t), tvl_usd)
    }

    /// APR for a pair's gauge pool, priced from the portfolio's own token prices.
    ///
    /// The denominator is everyone's stake, not ours: APR is the pool's, not this wallet's.
    pub fn pool_apr(&self, position: &wallet_core::liquidity::LpPosition) -> Option<f64> {
        let tvl = self.pool_tvl(&position.pair);
        match self.gauge_pool_for(&position.pair) {
            Some(gauge) => gauge.apr_from_tvl(wallet_core::registry::now(), &|t| self.token_usd(t), tvl),
            None => self.zone_apr(self.zone_pool_for(&position.pair)?, tvl),
        }
    }

    /// A pool's TVL from the markets list.
    pub fn pool_tvl(&self, pair: &str) -> Option<f64> {
        let Some(Ok((pools, _))) = self.eco.markets_view.pools.shown() else { return None };
        pools.iter().find(|p| p.address.eq_ignore_ascii_case(pair)).and_then(|p| p.tvl_usd)
    }

    /// What this wallet holds of a pool's token, for the deposit card's "have" figure.
    pub fn pool_token_balance(&self, token: &wallet_core::markets::PoolToken) -> Option<String> {
        let asset = SwapAsset::Token { address: token.address.clone(), symbol: token.symbol.clone(), decimals: token.decimals };
        let (amount, decimals) = self.exact_balance(&asset)?;
        Some(amount::group_thousands(&amount::format_amount_short(amount, decimals, 4)))
    }
}
