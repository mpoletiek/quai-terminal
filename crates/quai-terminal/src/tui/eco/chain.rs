//! System › Chain: the block headers this screen draws, asked for while it is open.
//!
//! The data worker reads them from the monitoring node when one is set, else the public RPC: one
//! poll every two seconds while the screen is open (the head's height, then the missing blocks in
//! one batch), backing off on failures. Nothing is asked while the screen is closed.

use super::*;
use std::collections::VecDeque;
use std::time::Duration;
use wallet_core::blocks::BlockHead;

/// Headers kept: more than the widest lattice draws.
const KEEP: usize = 128;
/// The first ask fills a lattice; later ones fill gaps.
const FIRST: u16 = 48;
const POLL: Duration = Duration::from_secs(2);
/// An answer that never came (no data worker, a hung read) stops blocking the next ask.
const STALE: Duration = Duration::from_secs(15);

#[derive(Default)]
pub struct ChainLog {
    /// Headers seen, oldest first; runs may have gaps between them.
    pub blocks: VecDeque<BlockHead>,
    /// When each height arrived here: the newest block is lit for a moment.
    pub arrived: HashMap<u64, Instant>,
    pub asking: Option<Instant>,
    pub last_ask: Option<Instant>,
    pub failures: u32,
    pub error: Option<String>,
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
        let (first, last) = (run.last()?, run.first()?);
        let n = run.len().checked_sub(1).filter(|n| *n > 0)? as f64;
        Some(last.timestamp.saturating_sub(first.timestamp) as f64 / n)
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

    /// Time for the next ask: none in flight (or the last one went stale), and the poll interval
    /// passed, doubled for each failure in a row up to a minute.
    pub fn due(&self, now: Instant) -> bool {
        if self.asking.is_some_and(|at| now.saturating_duration_since(at) < STALE) {
            return false;
        }
        let wait = POLL.saturating_mul(1 << self.failures.min(5)).min(Duration::from_secs(60));
        self.last_ask.is_none_or(|at| now.saturating_duration_since(at) >= wait)
    }
}

impl App {
    /// While System › Chain is open: ask for the blocks since the newest one held.
    pub fn tick_chain_heads(&mut self) {
        if self.eco.chain.network != self.dash.network_id {
            self.eco.chain = ChainLog { network: self.dash.network_id.clone(), ..ChainLog::default() };
        }
        let now = Instant::now();
        if !self.eco.chain.due(now) {
            return;
        }
        self.eco.chain.asking = Some(now);
        self.eco.chain.last_ask = Some(now);
        let after = self.eco.chain.newest().map(|b| b.height);
        let max = if after.is_some() { wallet_core::blocks::MAX_BATCH as u16 } else { FIRST };
        self.send_data(DataCmd::ChainHeads { after, max });
    }

    pub(crate) fn settle_chain_heads(&mut self, result: std::result::Result<Vec<BlockHead>, String>) {
        let chain = &mut self.eco.chain;
        chain.asking = None;
        match result {
            Ok(heads) => {
                chain.failures = 0;
                chain.error = None;
                if chain.merge(heads, Instant::now()) > 0 {
                    self.dirty = true;
                }
            }
            Err(e) => {
                chain.failures += 1;
                chain.error = Some(e);
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

    #[test]
    fn asks_wait_for_the_last_and_back_off_on_failures() {
        let now = Instant::now();
        let mut log = ChainLog { last_ask: Some(now), ..ChainLog::default() };
        assert!(!log.due(now + Duration::from_secs(1)));
        assert!(log.due(now + Duration::from_secs(2)));
        log.failures = 3;
        assert!(!log.due(now + Duration::from_secs(15)));
        assert!(log.due(now + Duration::from_secs(16)));
        log.asking = Some(now + Duration::from_secs(16));
        assert!(!log.due(now + Duration::from_secs(20)), "one in flight");
        assert!(log.due(now + Duration::from_secs(16) + STALE), "until it goes stale");
    }
}
