//! What a contract says about itself, and how much of it can be checked.
//!
//! Solidity appends a CBOR tail to the runtime bytecode naming the compiler and, when built with
//! `bytecodeHash: "ipfs"`, the CID of the metadata document — the standard-json holding the ABI,
//! the settings and (with `useLiteralContent`) the source. Quai *requires* that build setting, so
//! on this chain a properly deployed contract carries a pointer to its own ABI in its code.
//!
//! That gives a chain of custody with exactly one strong link and one weak one, and the difference
//! is the whole reason this module is careful:
//!
//! - **Strong, where it holds.** The CID comes out of the deployed bytecode, and a metadata
//!   document small enough to be a single IPFS block can be checked against it
//!   ([`ipfs::Cid::verifies_content`]), so for those the bytes a gateway returned are provably the
//!   ones the CID names and a hostile gateway can only fail to answer. A document too large to be
//!   one block names a tree rather than its own bytes and cannot be checked here; that case is
//!   carried as [`Metadata::checked_against_cid`] and must be *said*, never implied away.
//! - **Weak.** *Which* CID the bytecode carries is the contract author's choice. The compiler puts
//!   the real one there, but nothing on-chain forces that: a hostile contract can embed the CID of
//!   some innocuous contract's metadata and be described by a friendly, entirely wrong ABI. The
//!   metadata commits to *source*, not to the deployed runtime, and only a recompile — which the
//!   wallet cannot do — closes that gap.
//!
//! So an ABI from here is a **decoding aid, never an endorsement**, and everything built on it has
//! to keep saying so. Two checks give real evidence and are carried on [`Discovered`]:
//!
//! - [`Discovered::undeclared`] — selectors the runtime dispatches on that the ABI does not
//!   declare. The ABI is then incomplete at best and lying at worst.
//! - [`Discovered::verified`] — whether the explorer says someone recompiled the standard-json and
//!   matched it against the deployed bytecode. That is the check the wallet cannot do itself.

use crate::data::{DataCtx, Trust};
use crate::error::{CoreError, Result};
use crate::ipfs;
use quai_sdk::abi::{AbiInterface, StateMutability};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

/// Metadata documents are a few kilobytes; this is the ceiling before one is refused, well under
/// the single-block size that makes a document checkable against its CID at all.
pub const MAX_METADATA_BYTES: usize = ipfs::UNIXFS_BLOCK;

/// How long a discovered interface is kept. Metadata is immutable by CID, so the only thing that
/// can change is which CID the address carries — which is a code change, and code is re-read on
/// every path that feeds a review.
pub const CACHE_SECS: u64 = 7 * 86_400;

/// The compiler tail at the end of a contract's runtime bytecode.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Tail {
    /// The metadata CID, as a gateway wants it. Absent on a build that did not embed one.
    pub cid: Option<String>,
    /// `0.8.20`, when the tail names the compiler.
    pub solc: Option<String>,
}

/// A contract's metadata document, parsed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Metadata {
    /// The CID this came from, proven against the bytes.
    pub cid: String,
    /// `Messages`, from `settings.compilationTarget`.
    pub name: String,
    /// `contracts/Messages.sol`.
    pub source_path: String,
    /// `0.8.20+commit.a1b79de6`.
    pub compiler: String,
    /// The ABI as its JSON array, kept so it can be cached and re-parsed.
    pub abi: Value,
    /// Source text, when the build inlined it (Quai's deploy settings require this).
    pub source: Option<String>,
    /// Whether these bytes were checked against the CID the bytecode named, rather than taken on
    /// the gateway's word. False for a document too large to be one IPFS block, where the CID
    /// names a tree of blocks and there is nothing to check without walking it.
    ///
    /// Nothing may describe an ABI as verified without reading this: it is the difference between
    /// arithmetic and trusting whoever answered.
    pub checked_against_cid: bool,
}

impl Metadata {
    /// The ABI as something calls can be built from.
    pub fn interface(&self) -> Result<AbiInterface> {
        let bytes = serde_json::to_vec(&self.abi).map_err(|e| CoreError::Invalid(format!("contract ABI: {e}")))?;
        AbiInterface::from_json(&bytes).map_err(|e| CoreError::Invalid(format!("contract ABI: {e}")))
    }
}

/// Everything the wallet has established about an address as a call destination.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Discovered {
    /// Lowercase `0x…`.
    pub address: String,
    /// Runtime bytecode length. Zero means this is not a contract at all.
    pub code_len: usize,
    /// Keccak-256 of the runtime, as the chain reported it.
    pub code_hash: String,
    /// The compiler version the tail named, when it named one.
    pub solc: Option<String>,
    /// The metadata, when the tail carried a CID and it resolved and verified.
    pub metadata: Option<Metadata>,
    /// Why there is no metadata, in a form fit to show.
    pub metadata_error: Option<String>,
    /// Function selectors the runtime dispatches on that the ABI does not declare.
    pub undeclared: Vec<String>,
    /// Whether the explorer reports the contract as verified. `None` when it was not asked or
    /// could not answer — never treat that as "no".
    pub verified: Option<bool>,
}

impl Discovered {
    /// Whether there is code here at all.
    pub fn is_contract(&self) -> bool {
        self.code_len > 0
    }

    /// The sentence a review or a form puts next to the ABI, saying exactly what it is worth.
    /// Always present when there is an ABI: the caveat never gets dropped for a "good" contract.
    pub fn trust_note(&self) -> Option<String> {
        let metadata = self.metadata.as_ref()?;
        let mut note = format!("`{}` per the contract's own metadata", metadata.name);
        if !metadata.checked_against_cid {
            note.push_str(" (which the gateway served unverified)");
        }
        match self.verified {
            Some(true) => note.push_str(", and the explorer has recompiled it against this bytecode"),
            _ => note.push_str(" — self-declared, not checked against the deployed code"),
        }
        if !self.undeclared.is_empty() {
            note.push_str(&format!("; {} function(s) in the code are missing from it", self.undeclared.len()));
        }
        Some(note)
    }
}

/// Read the compiler tail off runtime bytecode.
///
/// The last two bytes are the big-endian length of a CBOR map before them. Anything unexpected is
/// simply "no tail" — this runs on arbitrary bytes from the chain and must never panic or reach
/// outside the slice.
pub fn tail(runtime: &[u8]) -> Tail {
    let Some(len_bytes) = runtime.len().checked_sub(2).map(|i| &runtime[i..]) else {
        return Tail::default();
    };
    let len = usize::from(u16::from_be_bytes([len_bytes[0], len_bytes[1]]));
    let Some(start) = runtime.len().checked_sub(len + 2) else {
        return Tail::default();
    };
    let mut out = Tail::default();
    for (key, value) in cbor_map(&runtime[start..runtime.len() - 2]) {
        match (key.as_str(), value) {
            ("ipfs", CborValue::Bytes(b)) => out.cid = ipfs::Cid::from_bytes(&b).map(|c| c.to_text()),
            // A three-byte `solc` is the version; a longer one is a pre-release string.
            ("solc", CborValue::Bytes(b)) if b.len() == 3 => out.solc = Some(format!("{}.{}.{}", b[0], b[1], b[2])),
            _ => {}
        }
    }
    out
}

/// Every 4-byte selector the runtime pushes, which is how Solidity's dispatcher compares the call
/// against each function it knows. Used only to spot functions an ABI leaves out: a selector here
/// that the ABI does not declare is evidence against the ABI, while a selector the ABI declares
/// and this misses is not evidence against the code (a dispatcher can be built other ways).
pub fn selectors(runtime: &[u8]) -> BTreeSet<[u8; 4]> {
    const PUSH4: u8 = 0x63;
    let mut out = BTreeSet::new();
    let mut i = 0;
    while i < runtime.len() {
        let op = runtime[i];
        if op == PUSH4 && i + 5 <= runtime.len() {
            out.insert([runtime[i + 1], runtime[i + 2], runtime[i + 3], runtime[i + 4]]);
            i += 5;
            continue;
        }
        // Step over the operand of any PUSH so its bytes are never read as opcodes.
        i += 1 + if (0x60..=0x7f).contains(&op) { usize::from(op - 0x5f) } else { 0 };
    }
    out
}

/// Fetch and check the metadata a CID names, through the ABI gateway.
pub async fn fetch_metadata(cid: &str) -> Result<Metadata> {
    let located = ipfs::locate(ipfs::Content::Abi, cid)?;
    let fetched = crate::http::get(&located.url, MAX_METADATA_BYTES).await?;
    // The CID came out of the bytecode, so bytes that do not match it are not this contract's
    // metadata, whoever served them.
    let checked = match located.verify.as_ref().map(|expected| expected.verifies_content(&fetched.bytes)) {
        Some(Some(false)) => {
            return Err(CoreError::Rejected(format!("{cid}: the gateway returned content that is not what the CID names")));
        }
        Some(Some(true)) => true,
        // A CID that commits to a tree rather than to these bytes: nothing was proven either way,
        // and the caller has to say so rather than imply it was.
        Some(None) | None => false,
    };
    let mut metadata = parse_metadata(cid, &fetched.bytes)?;
    metadata.checked_against_cid = checked;
    Ok(metadata)
}

/// Parse a metadata document. Separate from the fetch so it can be tested on a fixture.
pub fn parse_metadata(cid: &str, bytes: &[u8]) -> Result<Metadata> {
    let doc: Value = serde_json::from_slice(bytes).map_err(|e| CoreError::Invalid(format!("{cid}: not a metadata document ({e})")))?;
    let abi = doc
        .pointer("/output/abi")
        .cloned()
        .filter(Value::is_array)
        .ok_or_else(|| CoreError::Invalid(format!("{cid}: a metadata document with no ABI")))?;
    // `compilationTarget` is a one-entry map of source path to contract name.
    let (source_path, name) = doc
        .pointer("/settings/compilationTarget")
        .and_then(Value::as_object)
        .and_then(|m| m.iter().next())
        .map(|(path, name)| (path.clone(), name.as_str().unwrap_or_default().to_string()))
        .unwrap_or_default();
    let source = doc
        .pointer("/sources")
        .and_then(Value::as_object)
        .and_then(|m| m.get(&source_path).or_else(|| m.values().next()))
        .and_then(|s| s.get("content"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(Metadata {
        cid: cid.to_string(),
        name: crate::ops::sanitize_display(&name),
        source_path: crate::ops::sanitize_display(&source_path),
        compiler: doc.pointer("/compiler/version").and_then(Value::as_str).map(crate::ops::sanitize_display).unwrap_or_default(),
        abi,
        source,
        // Set by whoever fetched it; parsing bytes proves nothing about where they came from.
        checked_against_cid: false,
    })
}

impl DataCtx {
    /// Everything this address is, as a call destination: whether it is a contract, what ABI it
    /// names, and how much of that stands up.
    ///
    /// The code read is the same hardened observation [`crate::data::verify_pinned`] uses: the
    /// network's genesis is checked before the address is sent, and the head is re-read around the
    /// code. Under [`Trust::FirstHand`] nothing here is answered from a cache. The metadata *is* cached even then: it is pinned by a CID taken from the code that
    /// was just read, so a cache hit is the same document by construction.
    pub async fn discover_contract(&self, address: &str, trust: Trust) -> Result<Discovered> {
        self.online()?;
        let parsed = crate::chain::addr(address)?;
        let key = address.to_lowercase();
        // Against the trusted genesis, so a node on another network is refused before it learns
        // which address was asked about. The untrusted single observation sent the address first.
        let observation = self
            .node
            .provider
            .observe_contract_codes(self.network.genesis_hash()?, &[(parsed, None)], quai_sdk::provider::BlockTag::Latest)
            .await
            .map_err(|e| match e {
                quai_sdk::ProviderError::GenesisMismatch => {
                    CoreError::Rejected(format!("the node is not on network `{}`; refusing to read from it", self.network.id))
                }
                other => other.into(),
            })?
            .into_iter()
            .next()
            .ok_or_else(|| CoreError::Network("the node returned no code observation".into()))?;
        let runtime = observation.code.bytes.bytes();
        let mut found = Discovered {
            address: key.clone(),
            code_len: runtime.len(),
            code_hash: observation.code.hash.to_string(),
            solc: None,
            metadata: None,
            metadata_error: None,
            undeclared: Vec::new(),
            verified: None,
        };
        if runtime.is_empty() {
            return Ok(found);
        }
        let tail = tail(runtime);
        found.solc = tail.solc;
        let Some(cid) = tail.cid else {
            found.metadata_error = Some("this contract's bytecode does not name a metadata CID".into());
            return Ok(found);
        };
        match self.metadata_cached(&cid).await {
            Ok(metadata) => {
                found.undeclared = undeclared_selectors(&metadata, runtime);
                found.metadata = Some(metadata);
            }
            Err(e) => found.metadata_error = Some(e.to_string()),
        }
        // A display path may answer this from the explorer's cache; a review path asks again.
        found.verified = self.explorer_verified(&key, trust).await;
        Ok(found)
    }

    /// Metadata by CID, from the store when it is there.
    ///
    /// Only a document that was checked against its CID is cached, and only a cached document that
    /// still says so is used. A document the gateway served unverified is fetched again every
    /// time: caching it would turn one bad answer into a week of them, under a key that looks
    /// authoritative, including for reviews that asked for [`Trust::FirstHand`].
    async fn metadata_cached(&self, cid: &str) -> Result<Metadata> {
        let key = format!("abi:{cid}");
        if let Ok(Some((text, at))) = self.app.cache_get(&key)
            && crate::registry::now().saturating_sub(at) < CACHE_SECS
            && let Ok(metadata) = serde_json::from_str::<Metadata>(&text)
            && metadata.checked_against_cid
            && metadata.cid == cid
        {
            return Ok(metadata);
        }
        let metadata = fetch_metadata(cid).await?;
        if metadata.checked_against_cid
            && let Ok(text) = serde_json::to_string(&metadata)
        {
            let _ = self.app.cache_put(&key, &text);
        }
        Ok(metadata)
    }

    /// What the explorer says about verification. Never an error: not knowing is a normal answer
    /// and must not stop a send.
    async fn explorer_verified(&self, address: &str, trust: Trust) -> Option<bool> {
        let key = format!("verified:{}:{address}", self.network.id);
        if trust.may_cache()
            && let Ok(Some((text, at))) = self.app.cache_get(&key)
            && crate::registry::now().saturating_sub(at) < CACHE_SECS
        {
            return text.parse().ok();
        }
        if !self.policy.explorer {
            return None;
        }
        let verified = self.explorer.contract_verified(address).await.ok()?;
        let _ = self.app.cache_put(&key, &verified.to_string());
        Some(verified)
    }
}

/// Selectors the runtime dispatches on that the ABI does not declare, as `0x1234abcd`.
fn undeclared_selectors(metadata: &Metadata, runtime: &[u8]) -> Vec<String> {
    let Ok(interface) = metadata.interface() else { return Vec::new() };
    let declared: BTreeSet<[u8; 4]> = interface.functions().filter_map(|f| quai_sdk::abi::function_selector(f.signature()).ok()).collect();
    selectors(runtime).difference(&declared).map(|s| format!("0x{}", hex::encode(s))).collect()
}

// ------------------------------------------------------------------ CBOR

/// The few CBOR shapes a compiler tail uses.
enum CborValue {
    Bytes(Vec<u8>),
    Text(String),
    Other,
}

/// Decode a CBOR map of text keys, returning the pairs it could read. Bounded and total: anything
/// malformed ends the walk rather than failing, because this runs on bytes from the chain.
fn cbor_map(bytes: &[u8]) -> Vec<(String, CborValue)> {
    let mut out = Vec::new();
    let Some((major, count, mut rest)) = head(bytes) else { return out };
    if major != 5 {
        return out;
    }
    for _ in 0..count.min(16) {
        let Some((key, after_key)) = value(rest) else { break };
        let Some((v, after_value)) = value(after_key) else { break };
        if let CborValue::Text(key) = key {
            out.push((key, v));
        }
        rest = after_value;
    }
    out
}

/// The major type, its argument and the bytes after the header.
fn head(bytes: &[u8]) -> Option<(u8, u64, &[u8])> {
    let (first, rest) = bytes.split_first()?;
    let major = first >> 5;
    let extra = first & 0x1f;
    let (argument, rest) = match extra {
        0..=23 => (u64::from(extra), rest),
        24 => (u64::from(*rest.first()?), rest.get(1..)?),
        25 => (u64::from(u16::from_be_bytes(rest.get(..2)?.try_into().ok()?)), rest.get(2..)?),
        26 => (u64::from(u32::from_be_bytes(rest.get(..4)?.try_into().ok()?)), rest.get(4..)?),
        27 => (u64::from_be_bytes(rest.get(..8)?.try_into().ok()?), rest.get(8..)?),
        _ => return None,
    };
    Some((major, argument, rest))
}

/// One value and whatever follows it.
fn value(bytes: &[u8]) -> Option<(CborValue, &[u8])> {
    let (major, argument, rest) = head(bytes)?;
    let take = |rest: &'_ [u8]| -> Option<(Vec<u8>, usize)> {
        let len = usize::try_from(argument).ok()?;
        Some((rest.get(..len)?.to_vec(), len))
    };
    match major {
        2 => {
            let (bytes, len) = take(rest)?;
            Some((CborValue::Bytes(bytes), rest.get(len..)?))
        }
        3 => {
            let (bytes, len) = take(rest)?;
            Some((CborValue::Text(String::from_utf8(bytes).ok()?), rest.get(len..)?))
        }
        // Integers and simple values carry everything in the header; nothing else is expected here.
        0 | 1 | 7 => Some((CborValue::Other, rest)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tail of the message board on mainnet (`0x0077AD43…`), which is a stock solc 0.8.20
    /// build with Quai's required `bytecodeHash: "ipfs"`.
    const BOARD_TAIL: &str = "a2646970667358221220a2b1a1fc77c6cbda58759187ebbe6772f0dfda6523395e39f41da12baea66ecc64736f6c6343000814";

    #[test]
    fn a_contract_names_its_own_metadata_in_its_bytecode() {
        let mut runtime = vec![0x60, 0x80, 0x60, 0x40];
        let tail_bytes = hex::decode(BOARD_TAIL).unwrap();
        runtime.extend_from_slice(&tail_bytes);
        runtime.extend_from_slice(&(tail_bytes.len() as u16).to_be_bytes());
        let read = tail(&runtime);
        assert_eq!(read.cid.as_deref(), Some("QmZHjrbTYGTTNfL9SoX3E3MQf2PdpB7iVwrax8qBdzj7DV"));
        assert_eq!(read.solc.as_deref(), Some("0.8.20"));
    }

    /// Arbitrary bytes from the chain: never a panic, never a read past the end, just "no tail".
    #[test]
    fn bytecode_without_a_usable_tail_says_so() {
        assert_eq!(tail(&[]), Tail::default());
        assert_eq!(tail(&[0x00]), Tail::default());
        // A length that runs off the front of the code.
        assert_eq!(tail(&[0xff, 0xff]), Tail::default());
        // A well-formed length over bytes that are not CBOR.
        assert_eq!(tail(&[0x01, 0x02, 0x03, 0x00, 0x03]), Tail::default());
        // A tail naming a compiler but no CID is still read for what it does say.
        let solc_only = hex::decode("a164736f6c6343000814").unwrap();
        let mut runtime = solc_only.clone();
        runtime.extend_from_slice(&(solc_only.len() as u16).to_be_bytes());
        assert_eq!(tail(&runtime), Tail { cid: None, solc: Some("0.8.20".into()) });
    }

    /// Selectors come out of the dispatcher, and a PUSH's operand is never read as an opcode —
    /// otherwise any 32-byte constant containing `0x63` would invent selectors.
    #[test]
    fn selectors_are_read_from_the_dispatcher_only() {
        // PUSH4 aabbccdd, then PUSH4 11223344.
        let code = hex::decode("63aabbccdd6311223344").unwrap();
        let found = selectors(&code);
        assert_eq!(found.len(), 2);
        assert!(found.contains(&[0xaa, 0xbb, 0xcc, 0xdd]) && found.contains(&[0x11, 0x22, 0x33, 0x44]));
        // PUSH2 0x6300 — the 0x63 inside the operand is data, not a PUSH4.
        assert!(selectors(&hex::decode("616300").unwrap()).is_empty());
        // A truncated PUSH4 at the very end is not a selector.
        assert!(selectors(&hex::decode("63aabb").unwrap()).is_empty());
    }

    /// The real metadata document for the message board: the parts the wallet shows and calls with.
    #[test]
    fn a_metadata_document_gives_up_its_abi_and_name() {
        let doc = serde_json::json!({
            "compiler": {"version": "0.8.20+commit.a1b79de6"},
            "settings": {"compilationTarget": {"contracts/Messages.sol": "Messages"}},
            "sources": {"contracts/Messages.sol": {"content": "contract Messages {}"}},
            "output": {"abi": [{"type": "function", "name": "post", "inputs": [], "outputs": [], "stateMutability": "nonpayable"}]},
        });
        let parsed = parse_metadata("QmTest", &serde_json::to_vec(&doc).unwrap()).unwrap();
        assert_eq!((parsed.name.as_str(), parsed.source_path.as_str()), ("Messages", "contracts/Messages.sol"));
        assert_eq!(parsed.compiler, "0.8.20+commit.a1b79de6");
        assert_eq!(parsed.source.as_deref(), Some("contract Messages {}"));
        assert!(parsed.interface().unwrap().function("post").is_ok());
        // A document with no ABI is refused rather than becoming an empty interface.
        assert!(parse_metadata("QmTest", b"{\"compiler\":{}}").is_err());
        assert!(parse_metadata("QmTest", b"not json").is_err());
    }

    /// The caveat is attached to every ABI, and says something different once the explorer has
    /// recompiled the contract — but never disappears.
    #[test]
    fn an_abi_always_carries_what_it_is_worth() {
        let metadata = parse_metadata(
            "QmTest",
            &serde_json::to_vec(&serde_json::json!({
                "settings": {"compilationTarget": {"a.sol": "Token"}},
                "output": {"abi": []},
            }))
            .unwrap(),
        )
        .unwrap();
        let base = Discovered {
            address: "0x00".into(),
            code_len: 10,
            code_hash: String::new(),
            solc: None,
            metadata: Some(metadata),
            metadata_error: None,
            undeclared: Vec::new(),
            verified: None,
        };
        let unknown = base.trust_note().unwrap();
        assert!(unknown.contains("Token") && unknown.contains("self-declared"), "{unknown}");
        let verified = Discovered { verified: Some(true), ..base.clone() }.trust_note().unwrap();
        assert!(verified.contains("recompiled"), "{verified}");
        let lying = Discovered { undeclared: vec!["0xdeadbeef".into()], ..base.clone() }.trust_note().unwrap();
        assert!(lying.contains("missing from it"), "{lying}");
        // No metadata, no note to make.
        assert!(Discovered { metadata: None, ..base }.trust_note().is_none());
    }
}

// ------------------------------------------------------- calling one

/// A function the wallet is willing to offer on the send form, with what it needs to be called.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Callable {
    /// `transfer(address,uint256)` — what identifies it, since a name can be overloaded.
    pub signature: String,
    /// `transfer`.
    pub name: String,
    /// One entry per argument: the label to show and the type to parse against.
    pub inputs: Vec<(String, String)>,
    /// Whether it takes QUAI alongside the call.
    pub payable: bool,
    /// Whether it only reads. Those are answered rather than signed.
    pub read_only: bool,
}

impl Callable {
    /// `transfer(to, amount)` — the one-line form for a picker.
    pub fn label(&self) -> String {
        let args: Vec<&str> = self.inputs.iter().map(|(name, ty)| if name.is_empty() { ty.as_str() } else { name.as_str() }).collect();
        let mut out = format!("{}({})", self.name, args.join(", "));
        if self.payable {
            out.push_str(" · payable");
        } else if self.read_only {
            out.push_str(" · read");
        }
        out
    }
}

/// Every function an ABI declares, reads first, each side alphabetical, so the list a user picks
/// from is stable rather than however the compiler happened to emit it.
pub fn callables(interface: &AbiInterface) -> Vec<Callable> {
    let mut out: Vec<Callable> = interface
        .functions()
        .map(|f| {
            // Through the enum, not its `Debug` text: a rename upstream would silently turn every
            // function into a non-payable write.
            let mutability = f.state_mutability();
            Callable {
                signature: f.signature().to_string(),
                name: f.name().to_string(),
                inputs: f.input_parameters().iter().map(|p| (p.name().to_string(), p.abi_type().canonical_name())).collect(),
                payable: mutability == StateMutability::Payable,
                read_only: matches!(mutability, StateMutability::Pure | StateMutability::View),
            }
        })
        .collect();
    out.sort_by(|a, b| b.read_only.cmp(&a.read_only).then_with(|| a.name.cmp(&b.name)).then_with(|| a.signature.cmp(&b.signature)));
    out
}

/// An array type split into its element type and its fixed length, if it has one: `uint256[3]`
/// becomes `("uint256", Some(3))` and `address[]` becomes `("address", None)`. `None` for anything
/// that is not an array — a tuple, or a scalar.
fn array_element(ty: &str) -> Option<(&str, Option<usize>)> {
    let inner = ty.strip_suffix(']')?;
    let open = inner.rfind('[')?;
    let (element, count) = inner.split_at(open);
    // A tuple's own brackets never reach here: `(a,b)[2]` splits at the last `[`, leaving the
    // tuple as the element type, which is exactly right.
    let count = &count[1..];
    if count.is_empty() {
        return Some((element, None));
    }
    Some((element, Some(count.parse().ok()?)))
}

/// One array element as the text [`parse_argument`] reads, so an element is held to the same rules
/// as the same value typed into a field of its own. A nested list stays JSON; anything that is
/// neither text, a number nor a list is refused rather than guessed at.
fn element_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(_) => Some(value.to_string()),
        _ => None,
    }
}

/// Turn what someone typed into the JSON the ABI coder wants for `ty`.
///
/// Deliberately strict. This is the last point where a typo is still cheap: an address that is one
/// character short, or a number with a stray comma, should be a message on the form rather than a
/// signed transaction that reverts or — worse — does something else than was meant.
pub fn parse_argument(ty: &str, text: &str) -> Result<Value> {
    let text = text.trim();
    let bad = |why: &str| CoreError::Invalid(format!("{ty}: {why}"));
    // Arrays and tuples are taken as JSON, which is the only unambiguous way to type one.
    if ty.ends_with(']') || ty.starts_with('(') {
        let value: Value = serde_json::from_str(text).map_err(|_| bad("expects a JSON list, e.g. [\"0x…\", \"1\"]"))?;
        let items = value.as_array().ok_or_else(|| bad("expects a JSON list"))?;
        // An array's elements are checked one at a time, against the element type, so a bad one is
        // named by its position rather than turning into "does not match its type" at encode time
        // with nothing to say which. A tuple's element types cannot be split off the type string
        // as cheaply, so those are left to the coder, which refuses them just as firmly.
        let Some((element, fixed)) = array_element(ty) else {
            return Ok(Value::Array(items.clone()));
        };
        if let Some(n) = fixed
            && items.len() != n
        {
            return Err(bad(&format!("takes exactly {n} item(s), got {}", items.len())));
        }
        let mut out = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let text = element_text(item).ok_or_else(|| bad(&format!("item {i} is not a value ({element} expected)")))?;
            out.push(parse_argument(element, &text).map_err(|e| bad(&format!("item {i}: {e}")))?);
        }
        return Ok(Value::Array(out));
    }
    if ty == "bool" {
        return match text.to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" => Ok(Value::Bool(true)),
            "false" | "no" | "0" => Ok(Value::Bool(false)),
            _ => Err(bad("expects true or false")),
        };
    }
    if ty == "address" {
        let parsed = crate::chain::addr(text)?;
        return Ok(Value::String(parsed.to_string()));
    }
    if ty == "string" {
        return Ok(Value::String(text.to_string()));
    }
    if ty.starts_with("bytes") {
        let body = text.strip_prefix("0x").unwrap_or(text);
        if !body.is_empty() && !body.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(bad("expects hex, e.g. 0x00ff"));
        }
        if !body.len().is_multiple_of(2) {
            return Err(bad("hex needs an even number of digits"));
        }
        // `bytes32` is exactly 32 bytes. Left to the coder, a short value is a length error at
        // encode time at best; said here, it is a typo the user can still see and fix.
        if let Ok(width) = ty.trim_start_matches("bytes").parse::<usize>()
            && body.len() != width * 2
        {
            return Err(bad(&format!("is exactly {width} bytes — {} hex digits, not {}", width * 2, body.len())));
        }
        return Ok(Value::String(format!("0x{body}")));
    }
    if ty.starts_with("uint") || ty.starts_with("int") {
        // A comma is a decimal separator across most of Europe, so deleting it would turn `1,5`
        // into `15` — a ten-fold error, on a value about to be signed, that the review would then
        // show back as the `1,5` the user typed. A decimal point is already refused below; a comma
        // is refused for the same reason rather than quietly reinterpreted as a group separator.
        if text.contains(',') {
            return Err(bad("expects a whole number in the smallest unit — no comma (`1,5` and `1.5` are both ambiguous here)"));
        }
        // Passed as text: the coder reads decimal or 0x, and a JSON number would silently lose
        // precision above 2^53 — which is most token amounts.
        let cleaned = text.replace(['_', ' '], "");
        if cleaned.is_empty() {
            return Err(bad("expects a whole number"));
        }
        let body = cleaned.strip_prefix('-').unwrap_or(&cleaned);
        let digits = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X"));
        let valid = match digits {
            Some(hex) => !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()),
            None => !body.is_empty() && body.chars().all(|c| c.is_ascii_digit()),
        };
        if !valid {
            return Err(bad("expects a whole number (no decimal point — use the smallest unit)"));
        }
        if cleaned.starts_with('-') && ty.starts_with("uint") {
            return Err(bad("cannot be negative"));
        }
        return Ok(Value::String(cleaned));
    }
    Err(bad("is a type this wallet cannot fill in yet"))
}

#[cfg(test)]
mod call_tests {
    use super::*;

    #[test]
    fn arguments_are_parsed_strictly_against_their_type() {
        assert_eq!(parse_argument("bool", "TRUE").unwrap(), Value::Bool(true));
        assert_eq!(parse_argument("bool", "0").unwrap(), Value::Bool(false));
        assert!(parse_argument("bool", "maybe").is_err());
        // Big amounts stay text: as a JSON number this would lose its low digits.
        // A comma is refused, not deleted: `1,5` is one-and-a-half to most of Europe, and turning
        // it into `15` would be a ten-fold error the review would confirm back as `1,5`.
        assert!(parse_argument("uint256", "1,5").is_err(), "a comma is ambiguous, not a group separator");
        assert!(parse_argument("uint256", "1,000,000").is_err());
        assert_eq!(parse_argument("uint256", "1_000_000").unwrap(), Value::String("1000000".into()));
        let huge = "123456789012345678901234567890";
        assert_eq!(parse_argument("uint256", huge).unwrap(), Value::String(huge.into()));
        assert_eq!(parse_argument("uint256", "0xff").unwrap(), Value::String("0xff".into()));
        assert!(parse_argument("uint256", "-1").is_err(), "unsigned");
        assert!(parse_argument("uint256", "1.5").is_err(), "no decimal point");
        assert!(parse_argument("int256", "-42").is_ok());
        assert_eq!(parse_argument("bytes", "00ff").unwrap(), Value::String("0x00ff".into()));
        assert!(parse_argument("bytes", "0xfff").is_err(), "odd digits");
        assert!(parse_argument("bytes32", "0xzz").is_err());
        // A fixed width is exactly that width.
        assert!(parse_argument("bytes32", "0x00").is_err(), "one byte is not 32");
        assert!(parse_argument("bytes32", &format!("0x{}", "ab".repeat(32))).is_ok());
        assert!(parse_argument("bytes", "0x00").is_ok(), "dynamic bytes take any length");
        assert_eq!(parse_argument("string", " hi ").unwrap(), Value::String("hi".into()));
        assert!(parse_argument("address", "not-an-address").is_err());
        assert!(parse_argument("uint256[]", "1,2,3").is_err(), "a list must be JSON");
        assert!(parse_argument("function", "x").is_err(), "unknown types are refused, not guessed");
    }

    /// An array is checked element by element, against the element type, so a bad one is named by
    /// its position — not left to surface at encode time as "does not match its type".
    #[test]
    fn array_elements_are_held_to_the_element_type() {
        // Elements are normalised exactly as the same value typed on its own would be.
        assert_eq!(parse_argument("uint256[]", "[\"1_000\", 2]").unwrap(), serde_json::json!(["1000", "2"]));
        let bad = parse_argument("uint256[]", "[\"1\", \"1.5\"]").unwrap_err().to_string();
        assert!(bad.contains("item 1") && bad.contains("decimal point"), "{bad}");
        // The same rules, so the same refusals.
        assert!(parse_argument("uint256[]", "[\"1,5\"]").is_err(), "a comma is ambiguous inside a list too");
        assert!(parse_argument("address[]", "[\"0x1\"]").is_err(), "a short address is refused inside a list");
        assert!(parse_argument("bytes32[]", "[\"0x00\"]").is_err(), "a fixed width applies inside a list");
        // A fixed-length array takes exactly that many.
        let arity = parse_argument("uint256[3]", "[\"1\", \"2\"]").unwrap_err().to_string();
        assert!(arity.contains("exactly 3"), "{arity}");
        assert!(parse_argument("uint256[3]", "[\"1\", \"2\", \"3\"]").is_ok());
        // Nested arrays recurse.
        assert_eq!(parse_argument("uint256[][2]", "[[\"1\"], [\"2\"]]").unwrap(), serde_json::json!([["1"], ["2"]]));
        assert!(parse_argument("uint256[][2]", "[[\"1\"]]").is_err(), "the outer arity still applies");
        // Something that is not a value at all is refused rather than passed on.
        assert!(parse_argument("uint256[]", "[{\"a\": 1}]").is_err());
        // A tuple is left to the coder, but still has to be a list.
        assert!(parse_argument("(address,uint256)", "[\"0x00\", \"1\"]").is_ok());
        assert!(parse_argument("(address,uint256)", "\"nope\"").is_err());
    }

    #[test]
    fn an_array_type_splits_into_its_element_and_length() {
        assert_eq!(array_element("uint256[]"), Some(("uint256", None)));
        assert_eq!(array_element("uint256[3]"), Some(("uint256", Some(3))));
        assert_eq!(array_element("uint256[][2]"), Some(("uint256[]", Some(2))));
        assert_eq!(array_element("(address,uint256)[2]"), Some(("(address,uint256)", Some(2))));
        assert_eq!(array_element("uint256"), None);
        assert_eq!(array_element("(address,uint256)"), None, "a tuple is not an array");
    }

    #[test]
    fn the_function_list_puts_reads_first_and_is_stable() {
        let abi = br#"[
            {"type":"function","name":"transfer","stateMutability":"nonpayable","inputs":[{"name":"to","type":"address"},{"name":"amount","type":"uint256"}],"outputs":[]},
            {"type":"function","name":"deposit","stateMutability":"payable","inputs":[],"outputs":[]},
            {"type":"function","name":"balanceOf","stateMutability":"view","inputs":[{"name":"who","type":"address"}],"outputs":[{"name":"","type":"uint256"}]}
        ]"#;
        let interface = AbiInterface::from_json(abi).unwrap();
        let list = callables(&interface);
        assert_eq!(list.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["balanceOf", "deposit", "transfer"]);
        assert!(list[0].read_only && !list[0].payable);
        assert!(list[1].payable);
        assert_eq!(list[2].label(), "transfer(to, amount)");
        assert_eq!(list[0].label(), "balanceOf(who) · read");
        assert_eq!(list[1].label(), "deposit() · payable");
        assert_eq!(list[2].inputs, vec![("to".to_string(), "address".to_string()), ("amount".to_string(), "uint256".to_string())]);
    }
}

#[cfg(test)]
mod gateway_tests {
    use super::*;

    /// A gateway that answers a metadata CID with something else is refused, and a gateway that
    /// answers honestly is believed. This is the one link in the chain that is arithmetic rather
    /// than trust, and it was silently doing nothing once: `locate` attached a verifier only for
    /// `raw` CIDs, and every solc metadata CID is `dag-pb`.
    // The lock only serialises tests over the process-wide gateway; this test's runtime is
    // single-threaded, and the other holders are plain synchronous tests.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn a_gateway_that_lies_about_metadata_is_refused() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let _gateway = crate::ipfs::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // The real metadata document for the mainnet message board, and its real CID.
        let honest = br#"{"compiler":{"version":"0.8.20+commit.a1b79de6"},"settings":{"compilationTarget":{"contracts/Messages.sol":"Messages"}},"output":{"abi":[]}}"#;
        let cid = {
            // The CID for exactly these bytes, computed the way IPFS would.
            let block = crate::ipfs::unixfs_block_for_test(honest);
            let digest = <sha2::Sha256 as sha2::Digest>::digest(&block);
            let mut mh = vec![0x12, 0x20];
            mh.extend_from_slice(&digest);
            crate::ipfs::Cid::from_bytes(&mh).unwrap().to_text()
        };

        let serve = |body: &'static [u8]| async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tokio::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    let mut buf = [0u8; 2048];
                    let _ = socket.read(&mut buf).await;
                    let head = format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(body).await;
                }
            });
            port
        };

        // An honest gateway: the document comes back and is marked as checked.
        let port = serve(honest).await;
        crate::ipfs::set_gateway(crate::ipfs::Content::Abi, Some(&format!("http://127.0.0.1:{port}"))).unwrap();
        let metadata = fetch_metadata(&cid).await.expect("the honest document is accepted");
        assert_eq!(metadata.name, "Messages");
        assert!(metadata.checked_against_cid, "an honest answer is recorded as checked");

        // A gateway serving a different ABI for the same CID is refused outright.
        let liar = br#"{"compiler":{"version":"0.8.20"},"settings":{"compilationTarget":{"evil.sol":"Evil"}},"output":{"abi":[]}}"#;
        let port = serve(liar).await;
        crate::ipfs::set_gateway(crate::ipfs::Content::Abi, Some(&format!("http://127.0.0.1:{port}"))).unwrap();
        let refused = fetch_metadata(&cid).await;
        assert!(matches!(refused, Err(CoreError::Rejected(_))), "a gateway substituted the ABI and was believed: {refused:?}");
        crate::ipfs::set_gateway(crate::ipfs::Content::Abi, None).unwrap();
    }
}
