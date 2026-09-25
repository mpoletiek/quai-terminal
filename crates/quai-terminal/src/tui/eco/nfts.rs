//! NFTs: loading holdings and the marketplace, listing, buying and transferring.

use super::*;

impl App {
    /// Fetch this wallet's own listings.
    pub fn load_my_listings(&mut self) {
        let sellers = self.owner_addresses();
        if !sellers.is_empty() {
            self.send_data(DataCmd::MyListings { sellers });
        }
    }

    /// This wallet's listing of an item, when the indexer has it.
    pub fn my_listing(&self, contract: &str, token_id: &str) -> Option<Listing> {
        match self.eco.nft.mine.latest() {
            Some(Ok(v)) => v.iter().find(|l| l.contract.eq_ignore_ascii_case(contract) && l.token_id == token_id).cloned(),
            _ => None,
        }
    }

    /// Recent NFT sales, as last read.
    pub fn nft_trades(&self) -> &[wallet_core::market::Trade] {
        self.eco.trades()
    }

    /// Keep the marketplace's own numbers current: floors and listing counts every 5 minutes,
    /// the sales history every 2. Both are one request for the whole market.
    pub fn load_nft_market(&mut self, force: bool) {
        let clock = self.eco.clock;
        if force {
            self.eco.nft.stats.begin(&clock);
            self.eco.nft.trades.begin(&clock);
        }
        if force || self.eco.nft.stats.take_due(fresh::NFT_STATS, &clock) {
            self.send_data(DataCmd::CollectionStats);
        }
        if force || self.eco.nft.trades.take_due(fresh::NFT_TRADES, &clock) {
            self.send_data(DataCmd::NftTrades);
        }
    }

    pub fn load_nfts(&mut self, refresh: bool) {
        let owners = self.owner_addresses();
        if owners.is_empty() || !self.config.features.nfts {
            return;
        }
        self.eco.nft.nfts.begin(&self.eco.clock);
        self.send_data(DataCmd::Nfts { owners, refresh });
    }

    pub(crate) fn explore_key(&mut self, key: KeyEvent) -> bool {
        if let Some(text) = &mut self.eco.nft.search {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    self.eco.nft.search_text = text.clone();
                    self.eco.nft.search = None;
                    self.nav.selected = 0;
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.eco.nft.search_text = text.clone();
                    self.nav.selected = 0;
                }
                KeyCode::Char(c) if text.len() < 40 => {
                    text.push(c);
                    self.eco.nft.search_text = text.clone();
                    self.nav.selected = 0;
                }
                _ => {}
            }
            return true;
        }
        match key.code {
            KeyCode::Char('/') => {
                self.eco.nft.search = Some(self.eco.nft.search_text.clone());
                true
            }
            KeyCode::Char('R') => {
                self.eco.nft.collections.begin(&self.eco.clock);
                self.send_data(DataCmd::Collections { query: None });
                self.load_nft_market(true);
                true
            }
            KeyCode::Char('S') => {
                self.eco.nft.sort = self.eco.nft.sort.next();
                self.nav.selected = 0;
                let by = self.eco.nft.sort.label();
                self.info(format!("collections by {by}"));
                true
            }
            _ => false,
        }
    }

    pub(crate) fn transfer_selected_nft(&mut self) {
        if let Some(Ok(items)) = self.eco.nft.nfts.latest()
            && let Some(n) = items.get(self.nav.selected)
        {
            let (c, id) = (n.item.contract.clone(), n.item.token_id.clone());
            self.open_nft_transfer(&c, &id);
        }
    }

    pub(crate) fn open_nft_transfer(&mut self, contract: &str, token_id: &str) {
        if !self.can_sign() {
            self.toast("this wallet is watch-only", true);
            return;
        }
        let held = matches!(self.eco.nft.nfts.latest(), Some(Ok(v)) if v.iter().any(|n| n.item.contract == contract && n.item.token_id == token_id));
        if !held {
            self.toast("this wallet does not hold that NFT", true);
            return;
        }
        let multi = matches!(self.eco.nft.nfts.latest(), Some(Ok(v)) if v.iter().any(|n| n.item.contract == contract && n.item.token_id == token_id && n.kind == wallet_core::explorer::TokenKind::Erc1155));
        self.open_form(FormKind::NftTransfer { contract: contract.to_string(), token_id: token_id.to_string(), multi });
    }

    /// Next step of buying the NFT in the top detail view (each step is its own review).
    pub fn buy_step(&mut self) {
        let Some(Detail::Nft(c, id)) = self.nav.detail.last().cloned() else { return };
        if !self.can_sign() {
            self.toast("this wallet is watch-only", true);
            return;
        }
        let listing = self.listing_for(&c, &id);
        match listing {
            Some(l) if !l.buyable() => {
                self.info(format!(
                    "Seaport listing — press {} to copy its Bazarr link",
                    super::super::keymap::key_of(super::super::keymap::Verb::CopyLink)
                ));
                return;
            }
            None => {
                self.toast("this item is not listed", true);
                return;
            }
            _ => {}
        }
        let account = self.dash.active_account().map(|a| a.address.clone());
        match self.eco.nft.asks.get(&(c.clone(), id.clone())) {
            None => self.info("checking the listing on-chain…"),
            Some(Err(e)) => {
                let e = e.clone();
                self.toast(e, true);
            }
            Some(Ok(check)) if !check.valid => {
                let problems = check.problems.join("; ");
                self.toast(format!("cannot buy: {problems}"), true);
            }
            Some(Ok(check)) => {
                let price = check.ask.as_ref().map(|a| a.price.clone());
                let name = self.listing_for(&c, &id).and_then(|l| l.name).unwrap_or_else(|| format!("#{id}"));
                self.start_steps(Prepare::NftBuyNext { account, contract: c, token_id: id, price }, format!("buy {name}"));
            }
        }
    }

    /// Buy a listing directly (from a listings table): the purchase sequence re-checks the ask
    /// on-chain before every step, at the price shown.
    pub fn buy_listing(&mut self, l: &Listing) {
        if !self.can_sign() {
            self.toast("this wallet is watch-only", true);
            return;
        }
        if !l.buyable() {
            self.info(format!(
                "Seaport listing — press {} to copy its Bazarr link",
                super::super::keymap::key_of(super::super::keymap::Verb::CopyLink)
            ));
            return;
        }
        let account = self.dash.active_account().map(|a| a.address.clone());
        let name = l.name.clone().unwrap_or_else(|| format!("#{}", l.token_id));
        self.start_steps(
            Prepare::NftBuyNext { account, contract: l.contract.clone(), token_id: l.token_id.clone(), price: Some(l.price.clone()) },
            format!("buy {name}"),
        );
    }

    /// Image URL for an NFT, from loaded metadata, holdings or listings.
    pub fn nft_image_url(&self, contract: &str, token_id: &str) -> Option<String> {
        let key = (contract.to_lowercase(), token_id.to_string());
        if let Some(Ok(item)) = self.eco.nft.meta.get(&key)
            && item.image.is_some()
        {
            return item.image.clone();
        }
        if let Some(Ok(v)) = self.eco.nft.nfts.latest()
            && let Some(n) = v.iter().find(|n| n.item.contract.eq_ignore_ascii_case(contract) && n.item.token_id == token_id)
            && n.item.image.is_some()
        {
            return n.item.image.clone();
        }
        self.listing_for(&key.0, token_id).and_then(|l| l.image)
    }

    /// Listing price with known token currencies named.
    pub fn listing_price(&self, l: &Listing) -> String {
        match self.config.network(&self.network_id) {
            Ok(n) => l.price_text_on(&n),
            Err(_) => l.price_text(),
        }
    }

    pub fn listing_for(&self, contract: &str, token_id: &str) -> Option<Listing> {
        self.eco
            .nft
            .listings
            .iter()
            .filter_map(|(_, r)| r.latest().and_then(|r| r.ok()))
            .flat_map(|v| v.iter())
            .find(|l| l.contract == contract && l.token_id == token_id)
            .cloned()
            .or_else(|| self.my_listing(contract, token_id))
    }

    /// Open the list-for-sale form (or change the price of this wallet's listing).
    pub fn open_nft_list(&mut self, contract: &str, token_id: &str) {
        if !self.can_sign() {
            self.toast("this wallet is watch-only", true);
            return;
        }
        let held = match self.eco.nft.nfts.latest() {
            Some(Ok(v)) => v.iter().find(|n| n.item.contract == contract && n.item.token_id == token_id).cloned(),
            _ => None,
        };
        let Some(held) = held else {
            self.toast("this wallet does not hold that NFT", true);
            return;
        };
        if held.kind != wallet_core::explorer::TokenKind::Erc721 {
            self.toast("only ERC-721 items can be listed on Zora asks (Bazarr lists ERC-1155 through Seaport)", true);
            return;
        }
        let network = self.net();
        let current = self.my_listing(contract, token_id).map(|l| {
            let (symbol, decimals) =
                network.as_ref().and_then(|n| wallet_core::market::known_currency(n, &l.currency)).unwrap_or(("QUAI", 18));
            (amount::format_amount(l.price_amount(), decimals), symbol.to_string())
        });
        self.open_form(super::super::app::FormKind::NftList {
            contract: contract.to_string(),
            token_id: token_id.to_string(),
            owner: held.owner.clone(),
            name: held.item.name.clone(),
            current,
        });
    }

    /// Cancel this wallet's listing of an item (one review; ownership re-checked on-chain).
    pub fn cancel_nft_listing(&mut self, contract: &str, token_id: &str) {
        if !self.can_sign() {
            self.toast("this wallet is watch-only", true);
            return;
        }
        let held = match self.eco.nft.nfts.latest() {
            Some(Ok(v)) => v.iter().find(|n| n.item.contract == contract && n.item.token_id == token_id).cloned(),
            _ => None,
        };
        let Some(held) = held else {
            self.toast("this wallet does not hold that NFT", true);
            return;
        };
        self.start_steps(
            Prepare::NftListNext {
                account: Some(held.owner.clone()),
                contract: contract.to_string(),
                token_id: token_id.to_string(),
                price: None,
                currency: "QUAI".into(),
            },
            format!("cancel the listing of {}", held.item.name),
        );
    }

    pub(crate) fn selected_nft(&self) -> Option<(String, String)> {
        match self.eco.nft.nfts.latest() {
            Some(Ok(items)) => items.get(self.nav.selected).map(|n| (n.item.contract.clone(), n.item.token_id.clone())),
            _ => None,
        }
    }
}
