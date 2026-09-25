//! What a send review says about where the money is going, before anything is signed.
//!
//! Address poisoning is the most common way people lose funds to a wallet that did nothing
//! wrong: an attacker sends dust (or a zero-value token transfer) *from* an address made to share
//! the first and last characters of one the victim uses, so it sits in the history next to the
//! real one; later the victim copies the wrong one. An address is too long to read, so people
//! check the ends — which is exactly what the attacker forged.
//!
//! So every send review checks the destination against what this wallet knows:
//!
//! - **lookalike** — it shares the start and end of a known address (a contact, one of the
//!   wallet's own accounts, somewhere it has sent before) but is not that address. Danger.
//! - **dust-only history** — the address has only ever *sent* this wallet tiny amounts and has
//!   never been sent to. The classic poisoning setup. Danger.
//! - **first time** — never sent to, not a contact, not one of the wallet's accounts. A plain note:
//!   most first sends are fine, but it is the moment to check.

use crate::journal::OpKind;
use serde::Serialize;

/// Where a known address came from, which is also what the warning names it by.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Source {
    /// One of this wallet's own accounts.
    Own(String),
    /// A contact, by name.
    Contact(String),
    /// An address this wallet has sent to before.
    SentTo,
}

/// An address this wallet knows, with where it knows it from.
#[derive(Clone, Debug)]
pub struct Known {
    /// Lowercase, `0x`-prefixed.
    pub address: String,
    pub source: Source,
}

/// Whether a review warning is one of the recipient checks above (poisoning, dust-only history,
/// first time), which must stay ahead of every other warning in a review.
pub fn is_recipient_warning(w: &str) -> bool {
    w.starts_with("possible address poisoning")
        || w.starts_with("this address has only ever sent you dust")
        || w.starts_with("first time sending")
}

/// Something that came in from an address, for the dust check.
#[derive(Clone, Debug)]
pub struct Received {
    /// Sender, lowercase.
    pub from: String,
    /// Whether it was a trivial amount: a zero-value transfer, or dust.
    pub dust: bool,
}

/// Characters compared at each end. Four hex characters is 65,536 possibilities per end — cheap
/// for an attacker to grind, which is exactly why a match with a different middle is suspicious.
pub const ENDS: usize = 4;

/// The warnings for sending to `to`, most serious first. Empty when the address is one this
/// wallet has sent to or holds as a contact or account, and nothing about it looks forged.
pub fn warnings(to: &str, known: &[Known], received: &[Received]) -> Vec<String> {
    let to = to.trim().to_lowercase();
    let mut out = Vec::new();
    let exact = known.iter().filter(|k| k.address == to).collect::<Vec<_>>();
    // A lookalike of something known — unless it *is* something known.
    if exact.is_empty()
        && let Some(like) = known.iter().find(|k| lookalike(&to, &k.address))
    {
        let what = match &like.source {
            Source::Own(label) => format!("your own account {label}"),
            Source::Contact(name) => format!("your contact {name}"),
            Source::SentTo => "an address you have sent to before".into(),
        };
        out.push(format!(
            "possible address poisoning: this address starts and ends like {what} ({}) but is a different address — compare every character",
            crate::session::short_address(&like.address)
        ));
    }
    let sent_before = exact.iter().any(|k| k.source == Source::SentTo);
    let from_them: Vec<&Received> = received.iter().filter(|r| r.from == to).collect();
    if !sent_before && !from_them.is_empty() && from_them.iter().all(|r| r.dust) {
        out.push(
            "this address has only ever sent you dust or zero-value transfers — a common way to plant a lookalike in your history".into(),
        );
    }
    if exact.is_empty() && out.is_empty() {
        out.push("first time sending to this address — check it against where you got it, not against your history".into());
    }
    out
}

/// Same ends, different middle. Quai addresses carry their zone in the first byte (`0x00…` on
/// Cyprus-1), which every address shares, so the start is compared after it.
fn lookalike(a: &str, b: &str) -> bool {
    let body = |s: &str| s.trim_start_matches("0x").get(2..).map(str::to_string);
    let (Some(a), Some(b)) = (body(a), body(b)) else { return false };
    if a == b || a.len() < ENDS * 2 || b.len() < ENDS * 2 {
        return false;
    }
    a[..ENDS] == b[..ENDS] && a[a.len() - ENDS..] == b[b.len() - ENDS..]
}

/// Whether an incoming transfer is a trivial amount: zero, or under a millionth of a whole token
/// at the token's own scale. An NFT is never dust — one token is the whole thing.
pub fn is_dust(a: &crate::appdb::Activity) -> bool {
    if !a.detail.token_id().is_null() {
        return false;
    }
    let decimals = a.detail.decimals().as_u64().unwrap_or(18).min(77) as i32;
    let value: f64 = a.amount.parse().unwrap_or(0.0);
    value == 0.0 || value / 10f64.powi(decimals) < 1e-6
}

impl crate::session::Session {
    /// [`warnings`] for this wallet: its accounts, its contacts, where it has sent before, and what
    /// it has received from whom.
    pub fn recipient_warnings(&self, to: &str) -> Vec<String> {
        let mut known: Vec<Known> = Vec::new();
        for a in &self.meta.quai_accounts {
            known.push(Known { address: a.address.to_lowercase(), source: Source::Own(a.label.clone()) });
        }
        if let Ok(contacts) = self.app.contacts() {
            for c in contacts {
                let mut addresses: Vec<String> = c.address.iter().cloned().collect();
                addresses.extend(self.app.contact_addresses(c.id).unwrap_or_default());
                for a in addresses {
                    known.push(Known { address: a.to_lowercase(), source: Source::Contact(c.name.clone()) });
                }
            }
        }
        let network = &self.network.id;
        for op in self.app.operations(network, 10_000).unwrap_or_default() {
            let signed = !matches!(op.status, crate::appdb::OpStatus::Prepared | crate::appdb::OpStatus::Cancelled);
            if signed
                && op.counterparty.starts_with("0x")
                && matches!(op.kind, OpKind::SendQuai | OpKind::SendQi | OpKind::SendToken | OpKind::NftTransfer)
            {
                known.push(Known { address: op.counterparty.to_lowercase(), source: Source::SentTo });
            }
        }
        // "Sent to before" comes only from the journal above: transactions this wallet signed.
        // An explorer row that says this account sent something is not proof of it. A token's
        // Transfer event names whatever `from` its contract likes, and a zero-value `transferFrom`
        // needs no approval, so a poisoner can plant "you sent to 0x4dd9…42e4" in the history.
        // Trusting that switched off the very warning this check exists for.
        let mut received = Vec::new();
        for a in self.app.activity(network, 5_000).unwrap_or_default() {
            let Some(other) = a.detail.counterparty().as_str().map(str::to_lowercase) else { continue };
            if a.direction != "out" {
                received.push(Received { from: other, dust: is_dust(&a) });
            }
        }
        warnings(to, &known, &received)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = "0x004dd9afaa2768642b5cde15c24f37bf19d842e4";
    // Same start after the zone byte (4dd9) and same end (42e4), different middle.
    const FORGED: &str = "0x004dd900000000000000000000000000000042e4";

    #[test]
    fn fixtures_are_real_address_lengths() {
        for a in [REAL, FORGED, CONTACT_LOOKALIKE] {
            assert_eq!(a.len(), 42, "{a}");
        }
    }

    const CONTACT_LOOKALIKE: &str = "0x0022aabb9999999999999999999999999999aabb";

    fn known() -> Vec<Known> {
        vec![
            Known { address: REAL.into(), source: Source::SentTo },
            Known { address: "0x0022aabbccddeeff00112233445566778899aabb".into(), source: Source::Contact("Alice".into()) },
        ]
    }

    #[test]
    fn a_forged_lookalike_is_named_as_poisoning() {
        let w = warnings(FORGED, &known(), &[]);
        assert!(w[0].starts_with("possible address poisoning"), "{w:?}");
        assert!(w[0].contains("an address you have sent to before"));
        // The same for a contact's lookalike, named by the contact.
        let w = warnings(CONTACT_LOOKALIKE, &known(), &[]);
        assert!(w[0].contains("your contact Alice"), "{w:?}");
    }

    #[test]
    fn a_known_address_passes_quietly() {
        assert!(warnings(REAL, &known(), &[]).is_empty(), "sent to before: nothing to say");
        assert!(warnings(&REAL.to_uppercase().replace("0X", "0x"), &known(), &[]).is_empty(), "case does not matter");
    }

    #[test]
    fn a_new_address_gets_a_first_time_note() {
        let w = warnings("0x0011111111111111111111111111111111111111", &known(), &[]);
        assert_eq!(w.len(), 1);
        assert!(w[0].starts_with("first time sending"));
    }

    #[test]
    fn an_address_that_only_ever_sent_dust_is_called_out() {
        let dust = vec![Received { from: FORGED.into(), dust: true }, Received { from: FORGED.into(), dust: true }];
        let w = warnings(FORGED, &known(), &dust);
        assert_eq!(w.len(), 2, "lookalike and dust: {w:?}");
        assert!(w[1].contains("only ever sent you dust"));
        // A real payment from someone is not dust.
        let paid = vec![Received { from: "0x0011111111111111111111111111111111111111".into(), dust: false }];
        let w = warnings("0x0011111111111111111111111111111111111111", &known(), &paid);
        assert!(w[0].starts_with("first time sending"), "{w:?}");
    }

    #[test]
    fn an_nft_is_never_dust() {
        let at = |amount: &str, detail: serde_json::Value| crate::appdb::Activity {
            network: "n".into(),
            key: "k".into(),
            direction: "in".into(),
            asset: "X".into(),
            amount: amount.into(),
            address: REAL.into(),
            tx_hash: None,
            block: None,
            detail: detail.into(),
            observed: 0,
        };
        assert!(!is_dust(&at("1", serde_json::json!({"token_id": "12"}))), "one NFT");
        assert!(is_dust(&at("0", serde_json::json!({"decimals": 18}))), "zero-value transfer");
        assert!(is_dust(&at("1000", serde_json::json!({"decimals": 18}))), "1e-15 of a token");
        assert!(!is_dust(&at("5000000", serde_json::json!({"decimals": 6}))), "5 USDT");
    }

    #[test]
    fn only_the_ends_after_the_zone_byte_count() {
        // Every Cyprus-1 address starts 0x00; that alone is not a resemblance.
        assert!(!lookalike("0x00aaaa1111111111111111111111111111111111", "0x00bbbb2222222222222222222222222222221111"));
        assert!(lookalike("0x00aaaa1111111111111111111111111111111111", "0x00aaaa2222222222222222222222222222221111"));
        assert!(!lookalike(REAL, REAL), "an address is not a lookalike of itself");
        assert!(!lookalike("0x00ab", "0x00ab"));
    }

    /// A forged explorer row saying this wallet sent to a lookalike does not make the lookalike
    /// trusted: sending to it still names it as possible poisoning. Only a signed send in the
    /// journal counts as "sent to before".
    #[test]
    fn a_forged_out_transfer_does_not_silence_the_poisoning_check() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
        let registry = crate::registry::Registry::fast(paths);
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let meta = registry.create_hd("main", phrase, "english", "", "password123", true).unwrap();
        let network = crate::network::NetworkProfile::builtins().into_iter().next().unwrap();
        let s = crate::session::Session::open(registry, crate::config::AppConfig::default(), meta, network).unwrap();
        let net = s.network.id.clone();
        // A real send to REAL.
        s.app
            .insert_operation(&crate::appdb::Operation {
                id: "aa000000000000000000000000000001".into(),
                network: net.clone(),
                kind: OpKind::SendQuai,
                store: "quai".into(),
                account: "0x00aa".into(),
                status: crate::appdb::OpStatus::Confirmed,
                tx_hash: Some("0x01".into()),
                asset: "QUAI".into(),
                amount: "1".into(),
                counterparty: REAL.into(),
                fee: String::new(),
                detail: serde_json::json!({}).into(),
                created: 1,
                updated: 1,
            })
            .unwrap();
        // The poisoner's planted row: a zero-value token "transfer" from this wallet to FORGED.
        s.app
            .record_activity(&crate::appdb::Activity {
                network: net,
                key: "tt:0xbad:0:out".into(),
                direction: "out".into(),
                asset: "USDT".into(),
                amount: "0".into(),
                address: "0x00aa".into(),
                tx_hash: Some("0xbad".into()),
                block: Some(1),
                detail: serde_json::json!({"counterparty": FORGED, "source": "explorer"}).into(),
                observed: 2,
            })
            .unwrap();
        let w = s.recipient_warnings(FORGED);
        assert!(w.first().is_some_and(|w| w.starts_with("possible address poisoning")), "{w:?}");
        assert!(s.recipient_warnings(REAL).iter().all(|w| !w.contains("poisoning")), "the real one stays trusted");
    }

    /// Every warning the recipient check writes is recognised as one, so reviews can keep them
    /// ahead of fee notes and batch output can lead with them.
    #[test]
    fn recipient_warnings_are_recognised() {
        let known = known();
        for w in warnings(FORGED, &known, &[]).iter().chain(&warnings("0x00ffffffffffffffffffffffffffffffffffffff", &known, &[])) {
            assert!(is_recipient_warning(w), "{w}");
        }
        let dust = [Received { from: "0x00dddddddddddddddddddddddddddddddddddddd".into(), dust: true }];
        for w in &warnings("0x00dddddddddddddddddddddddddddddddddddddd", &known, &dust) {
            assert!(is_recipient_warning(w), "{w}");
        }
        assert!(!is_recipient_warning("maximum fee is above your fee policy"));
    }
}
