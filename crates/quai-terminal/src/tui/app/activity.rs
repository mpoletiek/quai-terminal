//! The activity list: operations and observed transfers, newest first.

use super::*;

impl App {
    /// Merged, newest-first activity rows: (time, is_operation, index).
    /// Activity as listed: (time, is an operation, index), newest first. Rebuilt only when the
    /// operations, the activity or the filter change; it is asked for every loop iteration.
    pub fn activity_rows(&self) -> Vec<(u64, bool, usize)> {
        let filter = if self.screen == Screen::Activity { self.activity_filter } else { ActivityFilter::All };
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (filter as u8).hash(&mut h);
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

    pub(crate) fn build_activity_rows(&self, filter: ActivityFilter) -> Vec<(u64, bool, usize)> {
        let nft_kind = |k: &str| k.starts_with("nft");
        let trade_kind =
            |k: &str| k == "swap" || k.starts_with("curve") || k.starts_with("convert") || k.contains("wrap") || k.contains("claim");
        let mut rows: Vec<(u64, bool, usize)> = self
            .dash
            .ops
            .iter()
            .enumerate()
            .filter(|(_, o)| o.status != OpStatus::Cancelled)
            .filter(|(_, o)| match filter {
                ActivityFilter::All => true,
                ActivityFilter::Sends => o.kind.starts_with("send") || o.kind == "nft_transfer",
                ActivityFilter::Receipts => false,
                ActivityFilter::Trades => trade_kind(&o.kind),
                ActivityFilter::Nfts => nft_kind(&o.kind),
            })
            .map(|(i, o)| (o.created, true, i))
            .collect();
        rows.extend(
            self.dash
                .activity
                .iter()
                .enumerate()
                .filter(|(_, a)| {
                    let nft = a.detail["standard"].as_str().is_some_and(|s| s != "ERC-20");
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
