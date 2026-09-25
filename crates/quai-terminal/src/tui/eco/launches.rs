//! The launch zone and trading PnL.

use super::*;

impl App {
    /// Ask for the launch zone when it has never loaded or is a minute old (the indexer's own
    /// cache is a minute too), or now with `force`.
    /// Ask the wallet worker for PnL: on opening the screen when the last answer is older than
    /// [`PNL_TTL`], and on `R`. Trades move it, so a stale answer is re-read rather than kept.
    /// Nothing is marked loading without a worker to answer: the request would go nowhere.
    pub fn load_pnl(&mut self, force: bool) {
        if self.worker.is_none() {
            return;
        }
        let pnl = &mut self.eco.feeds.pnl;
        if force && !pnl.loading() {
            pnl.begin(&self.eco.clock);
        } else if !pnl.take_due(fresh::PNL, &self.eco.clock) {
            return;
        }
        self.send(Cmd::Pnl);
    }

    /// Positions the PnL screen lists, in its order.
    pub fn pnl_positions(&self) -> &[wallet_core::pnl::Position] {
        match self.eco.feeds.pnl.latest() {
            Some(Ok(p)) => &p.positions,
            _ => &[],
        }
    }

    pub(crate) fn pnl_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('R') => {
                self.load_pnl(true);
                self.info("re-reading trades and prices");
                true
            }
            // Trade the focused token against QUAI on the swap card.
            KeyCode::Char('t') => {
                let Some(p) = self.pnl_positions().get(self.nav.selected).cloned() else { return true };
                self.eco.swap.from = SwapAsset::Quai;
                self.eco.swap.to = Some(SwapAsset::Token { address: p.token.clone(), symbol: p.symbol.clone(), decimals: p.decimals });
                self.eco.swap.amount.clear();
                self.eco.swap.quote = None;
                self.eco.swap.field = 1;
                self.show_card(Card::Swap);
                self.info(format!("buy {} with QUAI · f flips to sell", p.symbol));
                true
            }
            _ => false,
        }
    }

    pub fn load_launches(&mut self, force: bool) {
        let launches = &mut self.eco.launch.list;
        if force {
            launches.begin(&self.eco.clock);
        } else if !launches.take_due(fresh::LAUNCHES, &self.eco.clock) {
            return;
        }
        self.send_data(DataCmd::Launches);
    }

    /// Keep the focused launch's curve fresh (every 15 s): a curve moves with every trade.
    pub(crate) fn tick_curve(&mut self) {
        let Some(l) = self.launch_rows().get(self.nav.selected).cloned() else { return };
        let Some(curve) = l.curve.clone().filter(|_| {
            l.phase == wallet_core::launches::Phase::Bonding || l.venue_kind == Some(wallet_core::capabilities::Family::HartiiCurve)
        }) else {
            return;
        };
        if self.eco.launch.curves.take_due(l.token.clone(), fresh::CURVE, &self.eco.clock) {
            let owners = self.dash.active_account().map(|a| vec![a.address.clone()]).unwrap_or_default();
            self.send_data(DataCmd::CurveMarket { token: l.token, curve, owners });
        }
    }

    /// The focused launch's curve, when it has been read.
    pub fn focused_curve(&self) -> Option<&wallet_core::curve::CurveMarket> {
        let token = self.launch_rows().get(self.nav.selected)?.token.clone();
        self.eco.launch.curves.value(&token)
    }

    /// The launches shown, newest first.
    pub fn launch_rows(&self) -> Vec<wallet_core::launches::Launch> {
        use wallet_core::launches::Phase;
        let listed: &[wallet_core::launches::Launch] = self.eco.launch.list.value().map_or(&[], Vec::as_slice);
        let mut rows: Vec<wallet_core::launches::Launch> = listed
            .iter()
            .filter(|l| match l.phase {
                // A launch that has left its curve and can be found on an exchange is a market,
                // not a launch: Markets carries it with depth, a chart and the tape, and its buy
                // key here only opens the swap card anyway. One that has left its curve and cannot
                // be found stays — it has nowhere else to appear, and `c` is the only way left to
                // claim credit out of a curve that has already graduated.
                Phase::Pooled | Phase::Graduated => !self.market_lists_token(&l.token),
                Phase::Bonding | Phase::Other => true,
            })
            .cloned()
            .collect();
        // Live curves first, nearest graduation at the top: the stage bar is what this screen is
        // for, and a curve at 90% is the row worth looking at.
        //
        // Phase leads the sort rather than the stage alone, because a token that has finished its
        // curve reads as 100% and would otherwise outrank every curve still raising — the screen
        // would open on a launch that is over. Anything past its curve that survived the filter
        // above (nowhere else to appear) sits below the live ones, and a launch whose stage could
        // not be read sits below those again rather than among them reading as 0%.
        let rank = |l: &wallet_core::launches::Launch| u8::from(l.phase != Phase::Bonding);
        rows.sort_by(|a, b| {
            rank(a)
                .cmp(&rank(b))
                .then(a.progress_bps.is_none().cmp(&b.progress_bps.is_none()))
                .then(b.progress_bps.unwrap_or(0).cmp(&a.progress_bps.unwrap_or(0)))
                .then(a.symbol.cmp(&b.symbol))
        });
        rows
    }

    /// The token the Launches cursor is on, read before the list changes under it.
    pub(crate) fn launch_under_cursor(&self) -> Option<String> {
        (self.nav.screen == Screen::Launches).then(|| self.launch_rows().get(self.nav.selected).map(|l| l.token.clone())).flatten()
    }

    /// Put the Launches cursor back on `token` once the list has changed. The list re-ranks as
    /// curves trade and as the directory claims launches for Markets; a cursor left on its row
    /// number would point the curve card and `b`/`S` at another token.
    pub(crate) fn keep_launch_cursor(&mut self, token: Option<String>) {
        if let Some(i) = token.and_then(|t| self.launch_rows().iter().position(|l| l.token == t)) {
            self.nav.selected = i;
        }
    }

    /// Whether the market directory already carries a pool holding this token, on any exchange.
    ///
    /// False while the directory is still loading, so a row is never hidden on the strength of
    /// data the wallet does not have yet: the list fills in and then settles, rather than starting
    /// short and growing.
    pub(crate) fn market_lists_token(&self, token: &str) -> bool {
        let Some(Ok((pools, _))) = self.eco.markets_view.pools.shown() else { return false };
        pools
            .iter()
            .filter(|p| p.venue.routable())
            .any(|p| p.token0.address.eq_ignore_ascii_case(token) || p.token1.address.eq_ignore_ascii_case(token))
    }

    pub(crate) fn launches_key(&mut self, key: KeyEvent) -> bool {
        use wallet_core::launches::Phase;
        match key.code {
            KeyCode::Char('R') => {
                self.load_launches(true);
                self.info("reloading launches");
                true
            }
            KeyCode::Char(c @ ('b' | 'S' | 'c')) => {
                let Some(l) = self.launch_rows().get(self.nav.selected).cloned() else { return true };
                let Some(curve) = l.curve.clone() else {
                    self.toast(format!("{} has no bonding curve", l.symbol), true);
                    return true;
                };
                if !self.can_sign() {
                    self.toast("this wallet is watch-only", true);
                    return true;
                }
                let (token, symbol) = (l.token.clone(), l.symbol.clone());
                match c {
                    'c' if l.venue_kind == Some(wallet_core::capabilities::Family::HartiiCurve) => {
                        self.info("Hartii pays QUAI directly; no separate claim is needed");
                    }
                    'c' => {
                        let account = self.dash.active_account().map(|a| a.address.clone());
                        self.send(Cmd::Prepare(Prepare::CurveClaim { account, token, symbol, curve }));
                    }
                    _ if l.phase != wallet_core::launches::Phase::Bonding
                        && l.venue_kind != Some(wallet_core::capabilities::Family::HartiiCurve) =>
                    {
                        self.toast(format!("{symbol} has left its curve ({}); c still claims any credit", l.phase.text()), true);
                    }
                    'b' => self.open_form(FormKind::CurveBuy { token, symbol, curve }),
                    _ => {
                        let held = self.focused_curve().map(|m| amount::format_amount(m.held, m.token_decimals)).unwrap_or_default();
                        self.open_form(FormKind::CurveSell { token, symbol, curve, held });
                    }
                }
                true
            }
            KeyCode::Char('t') | KeyCode::Enter => {
                let Some(l) = self.launch_rows().get(self.nav.selected).cloned() else { return true };
                if l.venue_kind == Some(wallet_core::capabilities::Family::HartiiCurve) {
                    if let Some(curve) = l.curve.clone() {
                        self.open_form(FormKind::CurveBuy { token: l.token, symbol: l.symbol, curve });
                    }
                    return true;
                }
                match l.phase {
                    // Pooled on the main AMM or graduated into the launch AMM: the swap card's
                    // router trades both.
                    Phase::Pooled | Phase::Graduated => {
                        self.eco.swap.from = SwapAsset::Quai;
                        self.eco.swap.to = Some(SwapAsset::Token { address: l.token.clone(), symbol: l.symbol.clone(), decimals: 18 });
                        self.eco.swap.amount.clear();
                        self.eco.swap.quote = None;
                        self.eco.swap.field = 1;
                        self.show_card(Card::Swap);
                        self.info(format!("buy {} with QUAI · f flips to sell", l.symbol));
                    }
                    Phase::Bonding => {
                        let curve = l.curve.clone().unwrap_or_default();
                        self.open_form(FormKind::CurveBuy { token: l.token.clone(), symbol: l.symbol.clone(), curve });
                    }
                    Phase::Other => self.toast(format!("{} has no market the wallet can trade", l.symbol), true),
                }
                true
            }
            _ => false,
        }
    }
}
