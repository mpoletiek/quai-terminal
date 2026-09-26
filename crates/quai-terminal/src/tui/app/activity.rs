//! The activity list: operations and observed transfers, newest first.

use super::*;
use wallet_core::journal::OpKind;

impl App {
    /// Merged, newest-first activity rows: (time, is_operation, index).
    /// Activity as listed: (time, is an operation, index), newest first. Rebuilt only when the
    /// operations, the activity or the filter change; it is asked for every loop iteration.
    pub fn activity_rows(&self) -> Vec<(u64, bool, usize)> {
        let filter = if self.nav.screen == Screen::Activity { self.nav.activity_filter } else { ActivityFilter::All };
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (filter as u8).hash(&mut h);
            self.activity_only().hash(&mut h);
            for o in &self.dash.ops {
                (o.id.as_str(), o.status.as_str(), o.created, o.kind.as_str()).hash(&mut h);
            }
            for a in &self.dash.activity {
                (a.key.as_str(), a.observed, a.direction.as_str()).hash(&mut h);
            }
            h.finish()
        };
        if let Some((k, rows)) = self.activity_cache.borrow().as_ref()
            && *k == key
        {
            return rows.clone();
        }
        let rows = self.build_activity_rows(filter);
        *self.activity_cache.borrow_mut() = Some((key, rows.clone()));
        rows
    }

    /// The account activity is narrowed to (the one that acts), unless `.` asked for every
    /// account, lowercase. Everywhere activity is listed, Home included.
    fn activity_only(&self) -> Option<String> {
        self.nav.activity_account_only.then(|| self.dash.active_account().map(|a| a.address.to_lowercase())).flatten()
    }

    /// `.` on Activity: every account, or only the account that acts again.
    pub fn toggle_activity_account(&mut self) {
        self.nav.activity_account_only = !self.nav.activity_account_only;
        self.nav.selected = 0;
        let label = self.dash.active_account().map(|a| a.label.clone()).unwrap_or_default();
        self.toast(
            if self.nav.activity_account_only { format!("activity: {label} only") } else { "activity: every account".into() },
            false,
        );
    }

    pub(crate) fn build_activity_rows(&self, filter: ActivityFilter) -> Vec<(u64, bool, usize)> {
        let only = self.activity_only();
        // Qi belongs to the wallet, not to a Quai account: its operations and addresses show
        // whichever account acts.
        let accounts: Vec<String> = self.dash.accounts.iter().map(|a| a.address.to_lowercase()).collect();
        let mine =
            |address: &str| only.as_deref().is_none_or(|a| address.eq_ignore_ascii_case(a)) || !accounts.contains(&address.to_lowercase());
        let mut rows: Vec<(u64, bool, usize)> = self
            .dash
            .ops
            .iter()
            .enumerate()
            .filter(|(_, o)| o.status != OpStatus::Cancelled && mine(&o.account))
            .filter(|(_, o)| match filter {
                ActivityFilter::All => true,
                ActivityFilter::Sends => o.kind.is_send() || o.kind == OpKind::NftTransfer,
                ActivityFilter::Receipts => false,
                ActivityFilter::Trades => o.kind.is_trade(),
                ActivityFilter::Nfts => o.kind.is_nft(),
            })
            .map(|(i, o)| (o.created, true, i))
            .collect();
        rows.extend(
            self.dash
                .activity
                .iter()
                .enumerate()
                .filter(|(_, a)| mine(&a.address))
                .filter(|(_, a)| {
                    let nft = a.detail.standard().as_str().is_some_and(|s| s != "ERC-20");
                    match filter {
                        ActivityFilter::All => true,
                        ActivityFilter::Sends => a.direction == "out",
                        ActivityFilter::Receipts => a.direction == "in",
                        ActivityFilter::Trades => false,
                        ActivityFilter::Nfts => nft,
                    }
                })
                .map(|(i, a)| (a.observed, false, i)),
        );
        rows.sort_by(|a, b| b.0.cmp(&a.0));
        rows
    }

    /// Stable key for an activity row (for the detail stack).
    pub fn activity_key(&self, index: usize) -> Option<String> {
        match self.activity_rows().get(index) {
            Some((_, true, i)) => self.dash.ops.get(*i).map(|o| format!("op:{}", o.id)),
            Some((_, false, i)) => self.dash.activity.get(*i).map(|a| format!("act:{}", a.key)),
            None => None,
        }
    }
}
