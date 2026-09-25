//! Remote values as resources: the value, when it was read, whether a read is in flight, and the
//! error the last one ended with — one field, not four.
//!
//! When a resource is read again is its [`Freshness`], and every resource's class is in
//! [`fresh`]: `docs/INFORMATION_HIERARCHY.md`'s table ("what moves each screen") in one place.
//! Code that shows a resource asks [`Resource::take_due`]; it never keeps a clock of its own.
//!
//! Chain-backed values are measured against the [`Clock`]: a value read at an older block is
//! stale once a newer block arrives, and a timer is only the net under blocks that stopped
//! coming. A block therefore needs no list of what to refresh: each resource knows which block
//! it was read at.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// When a resource is read again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
    /// Chain state: at every new block. Without blocks (the head stream stopped, or has not
    /// started), every [`BLOCK_POLL`]; with them, [`BLOCK_NET`] at the latest.
    Block,
    /// A feed with its own window: after this long.
    Ttl(Duration),
    /// A feed with its own window, asked again sooner while its last read failed: `(window,
    /// retry)`.
    TtlRetry(Duration, Duration),
    /// Only when asked ([`Resource::invalidate`], or a first read).
    Manual,
}

impl Freshness {
    /// How long a read stays fresh by the clock alone, when the class has a window.
    pub fn window(self) -> Option<Duration> {
        match self {
            Freshness::Ttl(ttl) | Freshness::TtlRetry(ttl, _) => Some(ttl),
            Freshness::Block => Some(BLOCK_NET),
            Freshness::Manual => None,
        }
    }
}

/// Blocks count as arriving while the last one is younger than this.
pub const BLOCK_NET: Duration = Duration::from_secs(20);

/// How often chain state is read while no blocks arrive: the zone's block time.
pub const BLOCK_POLL: Duration = Duration::from_secs(wallet_core::markets::MARKET_TICK_SECS);

/// A read still unanswered after this is taken as lost, and asked again. Every source gives up
/// well before it (the HTTP client after 20 s, a slow venue after its deadline).
pub const STUCK: Duration = Duration::from_secs(45);

/// Every resource's class. Change a screen's pace here, and the table in
/// `docs/INFORMATION_HIERARCHY.md` with it.
pub mod fresh {
    use super::Freshness::{self, Block, Manual, Ttl};
    use std::time::Duration;

    const fn secs(n: u64) -> Freshness {
        Ttl(Duration::from_secs(n))
    }

    /// Home's value and holdings: amounts move with blocks (the dashboard), prices every minute.
    pub const PORTFOLIO: Freshness = secs(60);
    /// Network: the explorer's own five-minute window.
    pub const CHAIN_STATS: Freshness = secs(300);
    /// NFT collection statistics, and the trade tape: the marketplace's caches.
    pub const NFT_STATS: Freshness = secs(300);
    pub const NFT_TRADES: Freshness = secs(120);
    /// What the wallet owns, collections, listings: read on opening, and on request.
    pub const NFTS: Freshness = Manual;
    pub const COLLECTIONS: Freshness = Manual;
    pub const LISTINGS: Freshness = Manual;
    /// PnL: every 30 s while on screen.
    pub const PNL: Freshness = secs(30);
    /// The launch index, 15 s; each curve with the chain.
    pub const LAUNCHES: Freshness = secs(wallet_core::launches::LAUNCH_TTL);
    pub const CURVE: Freshness = Block;
    /// Markets: the pool directory every 30 s; reserves, the tape and LP positions with the chain.
    pub const MARKET_DIRECTORY: Freshness =
        Freshness::TtlRetry(Duration::from_secs(wallet_core::markets::DIRECTORY_TTL), super::BLOCK_POLL);
    /// A pair's ready-made candles from the indexer, while its chart is on screen.
    pub const CANDLES: Freshness = secs(5);
    /// A pair's own trades (its chart and tape).
    pub const POOL_EVENTS: Freshness = Block;
    pub const RESERVES: Freshness = Block;
    pub const DEX_FLOW: Freshness = Block;
    pub const LP_POSITIONS: Freshness = Block;
    /// Price alerts are checked every minute (the daemon checks them too).
    pub const ALERTS: Freshness = secs(60);
    /// A transaction's cost, once per transaction.
    pub const TX_COST: Freshness = Manual;
    /// The board: channels and sealed conversations move with the chain.
    pub const BOARD: Freshness = Block;
    /// Which channels are on the board: every 5 s while the Board is open, else every 45 s.
    pub const BOARD_SCAN_OPEN: Freshness = secs(5);
    pub const BOARD_SCAN: Freshness = secs(45);
    /// Private messages: an open conversation every 10 s, a pinned one every 15 s, the list
    /// every 30 s.
    pub const MESSAGES_OPEN: Freshness = secs(10);
    pub const MESSAGES_PINNED: Freshness = secs(15);
    pub const MESSAGES: Freshness = secs(30);
    /// Subscribed chats' news, while no daemon reads them.
    pub const CHAT_NEWS: Freshness = secs(30);
    /// A swap quote: 20 s, and 6 s while an approval is being mined (the quote says when it is
    /// no longer needed). A review is only built from a quote inside its window.
    pub const QUOTE: Freshness = secs(20);
    pub const QUOTE_APPROVING: Freshness = secs(6);
    /// Both QUAI ⇄ Qi markets for the convert card.
    pub const QI_ROUTES: Freshness = secs(20);
    /// A picture that failed to load is asked again after this; after a passing failure (a busy
    /// request budget, a rate limit, a timeout), sooner.
    pub const IMAGE_RETRY: Duration = Duration::from_secs(30);
    pub const IMAGE_RETRY_SOON: Duration = Duration::from_secs(4);
    /// The wallet worker's refresh stages. The node's height and QUAI balances run at every
    /// block; the rest move at the speed they can change at. A refresh the user asked for, and
    /// the one after a commit, ignore these.
    pub mod stage {
        use std::time::Duration;

        /// Token and wrapped balances: one multicall, so every block, like QUAI.
        pub const TOKENS_EVERY: Duration = Duration::from_secs(5);
        pub const QI_EVERY: Duration = Duration::from_secs(15);
        /// The price feed's own cache holds a minute; asking more often only reads the same answer.
        pub const PRICE_EVERY: Duration = Duration::from_secs(60);
        /// Locked conversion balances: three calls each, and they only move when a conversion settles.
        pub const LOCKED_EVERY: Duration = Duration::from_secs(60);
        /// The node's gas price, client version and block order: System-screen detail, not per-block news.
        pub const NODE_DETAIL_EVERY: Duration = Duration::from_secs(60);
        /// How long the wallet waits before refreshing on its own when no block has arrived.
        pub const IDLE_REFRESH: Duration = Duration::from_secs(15);
        /// Reconciling open operations and observing incoming activity.
        pub const TRACK_EVERY: Duration = Duration::from_secs(5);
        /// Scanning payment channels for senders never seen before.
        pub const PAYMENT_SYNC_EVERY: Duration = Duration::from_secs(90);
    }

    /// Limit orders: re-checked against fresh quotes at the daemon's pace.
    pub const ORDERS: Freshness = Ttl(crate::orders::WATCH_EVERY);
    /// The alert list itself: read once, then kept by what the user changes.
    pub const ALERT_LIST: Freshness = Manual;
}

/// The chain's clock, as the screens know it: the newest block and when it arrived.
#[derive(Clone, Copy, Debug, Default)]
pub struct Clock {
    pub head: u64,
    pub head_at: Option<Instant>,
}

impl Clock {
    /// A newer block: true when it moved the clock.
    pub fn block(&mut self, height: u64) -> bool {
        if height <= self.head {
            return false;
        }
        self.head = height;
        self.head_at = Some(Instant::now());
        true
    }

    /// Whether blocks are arriving (the last one is recent).
    pub fn blocks_arriving(&self) -> bool {
        self.head_at.is_some_and(|at| at.elapsed() < BLOCK_NET)
    }
}

/// One remote value.
#[derive(Clone, Debug)]
pub struct Resource<T> {
    value: Option<T>,
    error: Option<String>,
    /// When the last read settled (value or error).
    settled_at: Option<Instant>,
    /// When the last read was asked, and at which block.
    asked_at: Option<Instant>,
    asked_head: u64,
    loading: bool,
}

impl<T> Default for Resource<T> {
    fn default() -> Self {
        Resource { value: None, error: None, settled_at: None, asked_at: None, asked_head: 0, loading: false }
    }
}

impl<T> Resource<T> {
    /// The value, if one was ever read (kept through later failures).
    pub fn value(&self) -> Option<&T> {
        self.value.as_ref()
    }

    pub fn value_mut(&mut self) -> Option<&mut T> {
        self.value.as_mut()
    }

    /// Why the last read failed, if it did.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The last read's outcome: its error if it failed, else the value (none before any read).
    pub fn latest(&self) -> Option<Result<&T, &str>> {
        match (&self.error, &self.value) {
            (Some(e), _) => Some(Err(e)),
            (None, Some(v)) => Some(Ok(v)),
            (None, None) => None,
        }
    }

    /// What to show: the last good value, kept through failed refreshes; the error only when no
    /// read ever succeeded.
    pub fn shown(&self) -> Option<Result<&T, &str>> {
        match (&self.value, &self.error) {
            (Some(v), _) => Some(Ok(v)),
            (None, Some(e)) => Some(Err(e)),
            (None, None) => None,
        }
    }

    /// Whether a read is in flight.
    pub fn loading(&self) -> bool {
        self.loading
    }

    /// Whether any read has settled yet.
    pub fn settled(&self) -> bool {
        self.settled_at.is_some()
    }

    /// How old the last settled read is.
    pub fn age(&self) -> Option<Duration> {
        self.settled_at.map(|at| at.elapsed())
    }

    /// Whether this resource should be read now, by its class. Never while a read is in flight,
    /// unless that read is lost ([`STUCK`]).
    pub fn due(&self, class: Freshness, clock: &Clock) -> bool {
        let now = Instant::now();
        if self.loading {
            return self.asked_at.is_none_or(|at| now.duration_since(at) >= STUCK);
        }
        // Asked and answered nothing yet counts as the last read.
        let Some(last) = self.settled_at.or(self.asked_at) else { return true };
        let age = now.duration_since(last);
        match class {
            Freshness::Manual => false,
            Freshness::Ttl(ttl) => age >= ttl,
            Freshness::TtlRetry(ttl, retry) => age >= if self.error.is_some() { retry } else { ttl },
            Freshness::Block => clock.head > self.asked_head || age >= if clock.blocks_arriving() { BLOCK_NET } else { BLOCK_POLL },
        }
    }

    /// [`Resource::due`], and if it is, mark the read begun: the caller asks now.
    pub fn take_due(&mut self, class: Freshness, clock: &Clock) -> bool {
        let due = self.due(class, clock);
        if due {
            self.begin(clock);
        }
        due
    }

    /// A read begins (asked by the user, or due).
    pub fn begin(&mut self, clock: &Clock) {
        self.loading = true;
        self.asked_at = Some(Instant::now());
        self.asked_head = clock.head;
    }

    /// A read ended. A failure keeps the value read before, and says why beside it.
    pub fn settle(&mut self, result: Result<T, String>) {
        self.loading = false;
        self.settled_at = Some(Instant::now());
        match result {
            Ok(value) => {
                self.value = Some(value);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Nothing needed reading this time (a watch with nothing to watch): wait a full window
    /// before asking again, as if a read had just settled.
    pub fn rest(&mut self) {
        let now = Instant::now();
        self.asked_at = Some(now);
        self.settled_at = Some(now);
        self.loading = false;
    }

    /// A read that found nothing new: settled now, without error, the value unchanged.
    pub fn confirm(&mut self) {
        self.loading = false;
        self.settled_at = Some(Instant::now());
        self.error = None;
    }

    /// The block the last read was asked at.
    pub fn asked_head(&self) -> u64 {
        self.asked_head
    }

    /// A value that did not come from a read of this resource (a cache, a write the user made).
    pub fn set(&mut self, value: T) {
        self.value = Some(value);
        self.error = None;
    }

    /// Read it again at the next chance, keeping what is shown meanwhile.
    pub fn invalidate(&mut self) {
        self.settled_at = None;
        self.asked_at = None;
        self.loading = false;
    }

    /// Forget it entirely (a wallet or network switch).
    pub fn clear(&mut self) {
        *self = Resource::default();
    }

    /// Make the last read look older, for hosts' tests of what becomes due.
    #[cfg(any(test, feature = "test-support"))]
    pub fn age_by(&mut self, by: Duration) {
        self.settled_at = self.settled_at.and_then(|at| at.checked_sub(by));
        self.asked_at = self.asked_at.and_then(|at| at.checked_sub(by));
    }

    /// Take the value out, leaving nothing.
    pub fn take(&mut self) -> Option<T> {
        self.value.take()
    }
}

/// Resources of one kind, by key (a curve per token, a cost per transaction).
#[derive(Clone, Debug)]
pub struct Keyed<K, T> {
    map: HashMap<K, Resource<T>>,
}

impl<K, T> Default for Keyed<K, T> {
    fn default() -> Self {
        Keyed { map: HashMap::new() }
    }
}

impl<K: std::hash::Hash + Eq + Clone, T> Keyed<K, T> {
    pub fn get<Q: std::hash::Hash + Eq + ?Sized>(&self, key: &Q) -> Option<&Resource<T>>
    where
        K: std::borrow::Borrow<Q>,
    {
        self.map.get(key)
    }

    /// The value at `key`, if one was read.
    pub fn value<Q: std::hash::Hash + Eq + ?Sized>(&self, key: &Q) -> Option<&T>
    where
        K: std::borrow::Borrow<Q>,
    {
        self.map.get(key).and_then(Resource::value)
    }

    pub fn entry(&mut self, key: K) -> &mut Resource<T> {
        self.map.entry(key).or_default()
    }

    /// [`Resource::take_due`] for `key`.
    pub fn take_due(&mut self, key: K, class: Freshness, clock: &Clock) -> bool {
        self.entry(key).take_due(class, clock)
    }

    /// [`Resource::settle`] for `key`.
    pub fn settle(&mut self, key: K, result: Result<T, String>) {
        self.entry(key).settle(result);
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// Read every one again at the next chance, keeping what is shown.
    pub fn invalidate_all(&mut self) {
        self.map.values_mut().for_each(Resource::invalidate);
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &Resource<T>)> {
        self.map.iter()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aged<T>(mut r: Resource<T>, by: Duration) -> Resource<T> {
        r.settled_at = r.settled_at.and_then(|at| at.checked_sub(by));
        r.asked_at = r.asked_at.and_then(|at| at.checked_sub(by));
        r
    }

    #[test]
    fn a_first_read_is_due_and_one_in_flight_is_not_until_it_is_lost() {
        let clock = Clock::default();
        let mut r: Resource<u32> = Resource::default();
        assert!(r.take_due(fresh::PORTFOLIO, &clock));
        assert!(r.loading());
        assert!(!r.due(fresh::PORTFOLIO, &clock), "one read at a time");
        let r = aged(r, STUCK);
        assert!(r.due(fresh::PORTFOLIO, &clock), "a lost read is asked again");
    }

    #[test]
    fn a_ttl_resource_waits_its_window_and_a_failure_keeps_the_value() {
        let clock = Clock::default();
        let mut r: Resource<u32> = Resource::default();
        r.begin(&clock);
        r.settle(Ok(7));
        assert!(!r.due(fresh::PNL, &clock));
        r.begin(&clock);
        r.settle(Err("explorer is down".into()));
        assert_eq!((r.value(), r.error()), (Some(&7), Some("explorer is down")), "the old value stays, the error beside it");
        let r = aged(r, Duration::from_secs(31));
        assert!(r.due(fresh::PNL, &clock));
        assert!(!r.due(Freshness::Manual, &clock), "manual waits to be asked");
    }

    #[test]
    fn chain_state_is_due_at_each_new_block_and_on_a_clock_only_without_blocks() {
        let mut clock = Clock::default();
        clock.block(100);
        let mut r: Resource<u32> = Resource::default();
        assert!(r.take_due(fresh::RESERVES, &clock));
        r.settle(Ok(1));
        assert!(!r.due(fresh::RESERVES, &clock), "read at this block");
        clock.block(101);
        assert!(r.due(fresh::RESERVES, &clock), "a newer block makes it stale");
        assert!(r.take_due(fresh::RESERVES, &clock));
        r.settle(Ok(2));
        // Blocks arriving: the clock is only the net under them.
        let r2 = aged(r.clone(), BLOCK_POLL);
        assert!(!r2.due(fresh::RESERVES, &clock));
        assert!(aged(r, BLOCK_NET).due(fresh::RESERVES, &clock));
        // No blocks: every poll.
        let quiet = Clock { head: 101, head_at: Instant::now().checked_sub(BLOCK_NET * 2) };
        let mut r: Resource<u32> = Resource::default();
        r.begin(&quiet);
        r.settle(Ok(3));
        assert!(!r.due(fresh::RESERVES, &quiet));
        assert!(aged(r, BLOCK_POLL).due(fresh::RESERVES, &quiet));
    }

    #[test]
    fn an_old_block_does_not_move_the_clock() {
        let mut clock = Clock::default();
        assert!(clock.block(5));
        assert!(!clock.block(5));
        assert!(!clock.block(4));
        assert_eq!(clock.head, 5);
    }

    #[test]
    fn keyed_resources_are_apart() {
        let clock = Clock::default();
        let mut curves: Keyed<String, u32> = Keyed::default();
        assert!(curves.take_due("a".into(), fresh::CURVE, &clock));
        assert!(curves.take_due("b".into(), fresh::CURVE, &clock));
        assert!(!curves.take_due("a".into(), fresh::CURVE, &clock));
        curves.settle("a".into(), Ok(1));
        assert_eq!(curves.value(&"a".to_string()), Some(&1));
        assert_eq!(curves.value(&"b".to_string()), None);
    }
}
