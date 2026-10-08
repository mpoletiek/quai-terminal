//! System › Chain: the block headers this screen draws, asked for while it is open.
//!
//! The data worker reads them from the monitoring node when one is set, else the public RPC. While
//! the screen is open it follows the node's new heads over its WebSocket, and each one announced
//! fetches the blocks missing up to it (the head's height, then the blocks in one batch). Without
//! a WebSocket it polls; with one, a slower poll catches anything a dropped notification missed.
//! The pace is `resource::fresh`'s. Nothing is asked while the screen is closed, and leaving it
//! ends the subscription.

use super::*;
use quai_engine::resource::{Resource, fresh};
use std::collections::VecDeque;
use wallet_core::blocks::BlockHead;

/// Headers kept: more than the widest lattice draws.
const KEEP: usize = 128;
/// The first ask fills a lattice; later ones fill gaps.
const FIRST: u16 = 48;

#[derive(Default)]
pub struct ChainLog {
    /// Headers seen, oldest first; runs may have gaps between them.
    pub blocks: VecDeque<BlockHead>,
    /// When each height arrived here: the newest block is lit for a moment.
    pub arrived: HashMap<u64, Instant>,
    /// The header reads (their pace, and the last one's error).
    pub heads: Resource<()>,
    /// The subscription to new heads: asked for, and why the last one ended if it failed.
    pub watch: Resource<()>,
    /// A subscription was asked for and not ended since.
    pub watching: bool,
    /// New heads are arriving by subscription.
    pub live: bool,
    /// The network these headers belong to.
    pub network: String,
}

impl ChainLog {
    pub fn newest(&self) -> Option<&BlockHead> {
        self.blocks.back()
    }

    /// Take headers in. A height already held with another hash means the chain reorganized: the
    /// old block and everything above it go. Returns how many were new.
    pub fn merge(&mut self, heads: Vec<BlockHead>, now: Instant) -> usize {
        let mut added = 0;
        for b in heads {
            if let Some(i) = self.blocks.iter().position(|x| x.height == b.height) {
                if self.blocks[i].hash == b.hash {
                    continue;
                }
                self.blocks.truncate(i);
            }
            let at = self.blocks.iter().position(|x| x.height > b.height).unwrap_or(self.blocks.len());
            self.arrived.insert(b.height, now);
            self.blocks.insert(at, b);
            added += 1;
        }
        while self.blocks.len() > KEEP {
            self.blocks.pop_front();
        }
        let oldest = self.blocks.front().map_or(0, |b| b.height);
        self.arrived.retain(|h, _| *h >= oldest);
        added
    }

    /// Average seconds between blocks, from the newest unbroken run's timestamps.
    pub fn avg_block_secs(&self) -> Option<f64> {
        let run: Vec<&BlockHead> = self.run().collect();
        let (oldest, newest) = (run.last()?, run.first()?);
        let n = run.len().checked_sub(1).filter(|n| *n > 0)? as f64;
        Some(newest.timestamp.saturating_sub(oldest.timestamp) as f64 / n)
    }

    /// The newest blocks without a gap between them, newest first.
    pub fn run(&self) -> impl Iterator<Item = &BlockHead> {
        let mut next: Option<u64> = None;
        self.blocks.iter().rev().take_while(move |b| {
            let ok = next.is_none_or(|n| b.height + 1 == n);
            next = Some(b.height);
            ok
        })
    }

    /// Why reading failed, if the last read or subscription did.
    pub fn error(&self) -> Option<&str> {
        self.heads.error()
    }
}

impl App {
    /// While System › Chain is open: follow new heads, and ask for the blocks since the newest one
    /// held when one is announced or a poll is due.
    pub fn tick_chain_heads(&mut self) {
        if self.eco.chain.network != self.dash.network_id {
            self.end_chain_watch();
            self.eco.chain = ChainLog { network: self.dash.network_id.clone(), ..ChainLog::default() };
        }
        let clock = self.eco.clock;
        if !self.eco.chain.live && self.eco.chain.watch.take_due(fresh::CHAIN_WATCH, &clock) {
            self.eco.chain.watching = true;
            self.send_data(DataCmd::ChainWatch(true));
        }
        let pace = if self.eco.chain.live { fresh::CHAIN_HEADS_LIVE } else { fresh::CHAIN_HEADS };
        if !self.eco.chain.heads.take_due(pace, &clock) {
            return;
        }
        let after = self.eco.chain.newest().map(|b| b.height);
        let max = if after.is_some() { wallet_core::blocks::MAX_BATCH as u16 } else { FIRST };
        self.send_data(DataCmd::ChainHeads { after, max });
    }

    /// Off System › Chain (or locked): stop following new heads.
    pub fn end_chain_watch(&mut self) {
        if std::mem::take(&mut self.eco.chain.watching) {
            self.eco.chain.live = false;
            self.eco.chain.watch.invalidate();
            self.send_data(DataCmd::ChainWatch(false));
        }
    }

    /// A new head was announced: on the screen, fetch up to it at once.
    pub(crate) fn chain_head(&mut self, height: u64) {
        if !self.eco.chain.watching {
            return;
        }
        self.eco.chain.live = true;
        if self.eco.chain.newest().is_none_or(|b| b.height < height) && !self.eco.chain.heads.loading() {
            self.eco.chain.heads.invalidate();
            if self.nav.screen == Screen::Chain && !self.lock.locked {
                self.tick_chain_heads();
            }
        }
    }

    /// The subscription ended (the node closed it, or it failed): polling carries on, and it is
    /// asked for again on `fresh::CHAIN_WATCH`'s pace.
    pub(crate) fn chain_watch_ended(&mut self, result: std::result::Result<(), String>) {
        self.eco.chain.live = false;
        self.eco.chain.watch.settle(result);
        self.dirty = true;
    }

    pub(crate) fn settle_chain_heads(&mut self, result: std::result::Result<Vec<BlockHead>, String>) {
        let chain = &mut self.eco.chain;
        match result {
            Ok(heads) => {
                chain.heads.settle(Ok(()));
                if chain.merge(heads, Instant::now()) > 0 {
                    self.dirty = true;
                }
            }
            Err(e) => {
                chain.heads.settle(Err(e));
                self.dirty = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(height: u64, hash: &str, timestamp: u64) -> BlockHead {
        BlockHead {
            height,
            prime: 1,
            region: 1,
            order: 2,
            hash: hash.into(),
            parent: String::new(),
            timestamp,
            txs: 0,
            etxs: 0,
            workshares: 0,
            gas_used: 0,
            gas_limit: 1,
            base_fee_wei: 0,
            miner: String::new(),
            difficulty: 0,
            entropy_mbits: 0,
        }
    }

    #[test]
    fn merge_orders_dedupes_and_follows_a_reorg() {
        let mut log = ChainLog::default();
        let now = Instant::now();
        assert_eq!(log.merge(vec![head(10, "a", 100), head(11, "b", 105)], now), 2);
        assert_eq!(log.merge(vec![head(11, "b", 105), head(9, "z", 95)], now), 1, "11 again is not new; 9 slots in first");
        assert_eq!(log.blocks.iter().map(|b| b.height).collect::<Vec<_>>(), vec![9, 10, 11]);
        // 10 comes back with another hash: 10 and 11 are replaced by the new branch.
        assert_eq!(log.merge(vec![head(10, "a2", 101)], now), 1);
        assert_eq!(log.blocks.iter().map(|b| b.hash.as_str()).collect::<Vec<_>>(), vec!["z", "a2"]);
    }

    #[test]
    fn the_average_comes_from_the_newest_unbroken_run() {
        let mut log = ChainLog::default();
        let now = Instant::now();
        log.merge(vec![head(1, "a", 0), head(2, "b", 100), head(10, "c", 1000), head(11, "d", 1004), head(12, "e", 1010)], now);
        assert_eq!(log.run().map(|b| b.height).collect::<Vec<_>>(), vec![12, 11, 10]);
        assert_eq!(log.avg_block_secs(), Some(5.0));
    }
}
