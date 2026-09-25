//! Plain-language meanings for the words the wallet uses, and where each one appears.
//!
//! The `?` overlay lists the terms on the screen in front; the palette finds any of them ("what is
//! slippage"); the glossary modal holds them all.

use super::app::{Card, Screen};
use super::keymap::Place;

/// A term, what it means in a sentence or two, and the screens that use it.
pub struct Term {
    pub word: &'static str,
    pub meaning: &'static str,
    pub screens: &'static [Place],
}

/// A screen by name, or one of the exchange's cards.
macro_rules! place {
    (Swap) => {
        Place::Card(Card::Swap)
    };
    (Convert) => {
        Place::Card(Card::Convert)
    };
    (Wrap) => {
        Place::Card(Card::Wrap)
    };
    (Channels) => {
        Place::Pane(Screen::Contacts, 1)
    };
    ($s:ident) => {
        Place::Screen(Screen::$s)
    };
}

macro_rules! term {
    ($word:literal, $meaning:literal, [$($s:ident),*]) => {
        Term { word: $word, meaning: $meaning, screens: &[$(place!($s)),*] }
    };
}

pub const TERMS: &[Term] = &[
    term!("QUAI", "Quai's account-based coin: it pays every fee and is what contracts and tokens run on.", [Home, Accounts, Swap, Convert]),
    term!(
        "Qi",
        "Quai's coin-based currency, held as separate coins of fixed sizes rather than one balance. Payments go to fresh addresses, so they are harder to link.",
        [Home, Qi, Convert, Wrap]
    ),
    term!(
        "Qi coins",
        "The individual pieces a Qi balance is made of, each a fixed denomination. Spending picks coins and returns change, like cash.",
        [Qi]
    ),
    term!(
        "conversion",
        "Changing QUAI into Qi (or back) at the protocol's own rate, in one transaction. What you receive is locked for a period before it can be spent.",
        [Convert, Accounts]
    ),
    term!("lock", "Coins that exist but cannot be spent until a set block; the wallet counts down to it.", [Accounts, Qi, Convert]),
    term!(
        "payment code",
        "A reusable code (BIP47) you can share instead of an address. Each payment to it lands on a new address only you can find.",
        [Qi, Contacts, Channels]
    ),
    term!(
        "notify",
        "A one-time transaction a sender makes before paying a payment code, so the receiver's wallet knows where to look.",
        [Contacts, Channels]
    ),
    term!(
        "channel",
        "A sender and receiver linked by a payment-code notification; payments between them arrive on fresh addresses.",
        [Channels]
    ),
    term!("aggregate", "Combine many small Qi coins into fewer, larger ones, so later payments need fewer inputs and smaller fees.", [Qi]),
    term!("sweep", "Move every Qi coin from an imported key or address into this wallet.", [Qi]),
    term!(
        "WQI",
        "Wrapped Qi: a token on the QUAI side backed 1:1 by Qi, so Qi can trade on exchanges. Unwrapping pays out whole Qi.",
        [Wrap, Swap, Markets]
    ),
    term!("WQUAI", "Wrapped QUAI: QUAI as a token (1:1), which is how exchanges trade it. Swaps wrap and unwrap for you.", [Wrap, Swap]),
    term!(
        "slippage",
        "The most the price may move against you between quote and execution. Beyond it the swap fails instead of filling.",
        [Swap, Convert, Pools]
    ),
    term!(
        "price impact",
        "How far your own trade moves the pool's price. Large in a small pool: you pay more for each unit you buy.",
        [Swap]
    ),
    term!("route", "The pools a swap passes through, e.g. WQI → WQUAI → USDT when no pool trades the pair directly.", [Swap]),
    term!(
        "approval",
        "Permission for a contract to move up to a set amount of one of your tokens. This wallet approves the exact amount, never unlimited, unless you ask.",
        [Swap, Pools, Launches]
    ),
    term!(
        "TVL",
        "Total value locked: what a pool holds in both tokens, in dollars. Deeper pools move less per trade.",
        [Markets, Pools, Swap]
    ),
    term!("liquidity position", "Your share of a pool's two tokens. It earns a cut of every trade in that pool.", [Pools]),
    term!(
        "impermanent loss",
        "If the two prices in a pool drift apart, a position is worth less than just holding both tokens: about 5.7% at a 2× move.",
        [Pools]
    ),
    term!("gauge", "A contract that pays reward tokens to liquidity positions staked in it.", [Pools]),
    term!("stake", "Deposit liquidity-pool tokens into a gauge to earn its rewards; unstake to take them back.", [Pools]),
    term!("harvest", "Claim the rewards a staked position has earned so far, without unstaking.", [Pools]),
    term!("incentivize", "Give reward tokens to a gauge, streamed to its stakers. It cannot be undone.", [Pools]),
    term!(
        "bonding curve",
        "How a new launch sells: the price rises by formula as more is bought. When it raises its target it graduates to an ordinary pool.",
        [Launches, Markets]
    ),
    term!(
        "graduation",
        "A launch that met its target moves from its bonding curve to a normal exchange pool (◈ in Markets).",
        [Launches, Markets]
    ),
    term!(
        "nonce",
        "An account's transaction counter. Each transaction takes the next number, so a stuck one holds up those after it.",
        [Accounts, Activity]
    ),
    term!(
        "gas",
        "The work a transaction asks of the network. The fee is gas used × gas price; the review shows the most it can be.",
        [Activity, Network]
    ),
    term!(
        "fee policy",
        "The range of fees normal for this network. A fee above it is shown in red, but still sends if you approve.",
        [Activity]
    ),
    term!(
        "speed up",
        "Re-send a waiting transaction with the same nonce and a higher fee, so miners prefer it. Only one of the two can land.",
        [Activity]
    ),
    term!("zone", "Quai runs as several chains; this wallet uses Cyprus-1, whose addresses start 0x00.", [Accounts, Network]),
    term!("watch-only", "A wallet that knows addresses but holds no keys: it can show everything and sign nothing.", [Home, Wallets]),
    term!(
        "dust",
        "A tiny or zero-value transfer. Unknown senders use it to plant lookalike addresses in your history.",
        [Home, Activity, Contacts]
    ),
    term!(
        "address poisoning",
        "An attacker sends dust from an address made to start and end like one you use, hoping you copy it later. Reviews warn when a destination looks like this.",
        [Contacts, Activity]
    ),
];

/// The terms a place uses, in glossary order.
pub fn for_screen(place: Place) -> Vec<&'static Term> {
    TERMS.iter().filter(|t| t.screens.contains(&place)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_unique_and_meanings_fit_a_line_or_two() {
        let mut seen = std::collections::HashSet::new();
        for t in TERMS {
            assert!(seen.insert(t.word.to_lowercase()), "{} twice", t.word);
            assert!(t.meaning.len() <= 170, "{}: keep it to a line or two", t.word);
            assert!(t.meaning.ends_with('.'), "{}", t.word);
        }
        assert!(!for_screen(Place::Card(Card::Swap)).is_empty());
    }
}
