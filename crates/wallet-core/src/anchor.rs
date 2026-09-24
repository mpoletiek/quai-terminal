//! State anchors: the block a proof is read against, and whether a second node vouches for it.
//!
//! A state proof shows what an account or a storage slot holds under a block's state root, and
//! quai-sdk recomputes that root into the block's header hash. That alone says the answer is
//! consistent with the header the serving node reported, which a dishonest node can make up. It
//! stops resting on one node's word when a second, independent node holds a header with the same
//! hash at that height. The wallet has one when a monitoring node is configured: the monitor serves
//! the reads, and the network's RPC, where transactions are sent, is its witness.
//!
//! An anchor is read once and shared for [`ANCHOR_REUSE`], so a review's contract checks pay for
//! one confirmation between them rather than one each.
use crate::error::CoreError;
use crate::network::{NetworkProfile, Node, ZONE};
use quai_sdk::provider::StateAnchor;
use quai_sdk::{BlockTag, ProviderError, U256};
use std::time::{Duration, Instant};

/// How much deeper to anchor when the witness does not hold the serving node's newest block, or
/// briefly holds a different one there.
pub const ANCHOR_RETRY_DEPTH: u64 = 4;

/// Most blocks an anchor may sit below the witness's head. A node answering from further back is
/// lagging, or replaying an old block whose state (an allowance since revoked, code since
/// replaced) is no longer true.
pub const ANCHOR_MAX_LAG: u64 = 8;

/// How long one anchor serves further proofs: about two blocks. Long enough that a review's
/// contract checks share one confirmation; short enough that nothing proven is older than the
/// recheck window of a plain read.
pub const ANCHOR_REUSE: Duration = Duration::from_secs(10);

/// How far an anchor is vouched for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirmation {
    /// A second, independent node holds the same header at this height, and its head is no more
    /// than [`ANCHOR_MAX_LAG`] blocks past it.
    Witnessed,
    /// Only the serving node's header: consistent with that node, not independent of it.
    NodeOnly,
}

impl Confirmation {
    /// How a review names it.
    pub fn text(self) -> &'static str {
        match self {
            Confirmation::Witnessed => "confirmed by two nodes",
            Confirmation::NodeOnly => "as this node reports",
        }
    }
}

/// A block to prove state at, and how far it is vouched for.
#[derive(Clone, Debug)]
pub struct Anchored {
    /// The anchor: genesis checked, header hash recomputed.
    pub anchor: StateAnchor,
    /// Whether a second node confirmed it.
    pub confirmation: Confirmation,
}

/// The node's current anchor: a recent one while it lasts, otherwise a fresh read.
pub async fn anchor(node: &Node, network: &NetworkProfile) -> std::result::Result<Anchored, ProviderError> {
    if let Some(recent) = node.recent_anchor(ANCHOR_REUSE) {
        return Ok(recent);
    }
    let started = Instant::now();
    let fresh = read(node, network).await?;
    crate::diag::timing(
        match fresh.confirmation {
            Confirmation::Witnessed => "anchor.witnessed",
            Confirmation::NodeOnly => "anchor.node_only",
        },
        started,
    );
    node.remember_anchor(&fresh);
    Ok(fresh)
}

async fn read(node: &Node, network: &NetworkProfile) -> std::result::Result<Anchored, ProviderError> {
    let genesis = network.genesis_hash().map_err(|_| ProviderError::InvalidRequest("network has no trusted genesis"))?;
    let first = node.provider.state_anchor(genesis, ZONE, BlockTag::Latest).await?;
    let Some(witness) = node.witness() else {
        return Ok(Anchored { anchor: first, confirmation: Confirmation::NodeOnly });
    };
    match witnessed(witness, &first).await {
        Ok(()) => Ok(Anchored { anchor: first, confirmation: Confirmation::Witnessed }),
        // The witness has not reached this block yet, or holds another one at its height for a
        // moment: both are races at the head, and a few blocks down settles them.
        Err(ProviderError::ObservationChanged | ProviderError::AnchorDisputed) => {
            let deeper = BlockTag::Number(U256::from(first.block.number.saturating_sub(ANCHOR_RETRY_DEPTH)));
            let anchor = node.provider.state_anchor(genesis, ZONE, deeper).await?;
            witnessed(witness, &anchor).await?;
            Ok(Anchored { anchor, confirmation: Confirmation::Witnessed })
        }
        Err(error) => Err(error),
    }
}

/// The witness's header at the anchor's height must hash the same, and its head must not be more
/// than [`ANCHOR_MAX_LAG`] past the anchor. Both reads name no address, and they run together.
async fn witnessed(witness: &Node, anchor: &StateAnchor) -> std::result::Result<(), ProviderError> {
    let (confirmed, head) = tokio::join!(witness.provider.confirm_anchor(anchor), witness.provider.latest_header(ZONE));
    confirmed?;
    if let Some(head) = head?.map(|h| h.number) {
        anchor.require_number_at_least(head.saturating_sub(ANCHOR_MAX_LAG))?;
    }
    Ok(())
}

/// Whether the serving node's header carries a field this SDK cannot hash yet. That follows a
/// go-quai upgrade and is not the node's fault: a caller with another way to check falls back to
/// it rather than refusing.
pub fn unknown_header(error: &ProviderError) -> bool {
    matches!(error, ProviderError::Header(quai_sdk::provider::header_hash::HeaderHashError::UnknownField(_)))
}

/// What an anchoring failure means to someone reading a review, for the ones that are not a
/// passing network problem.
pub fn explain(error: &ProviderError, what: &str, network: &NetworkProfile) -> CoreError {
    match error {
        ProviderError::GenesisMismatch => {
            CoreError::Rejected(format!("{what}: the node is not on network `{}`; refusing to read from it", network.id))
        }
        ProviderError::AnchorDisputed => CoreError::Rejected(format!(
            "{what}: your monitoring node and {} disagree about the chain at the same height; refusing to trust either for this",
            crate::network::rpc_origin(&network.rpc_url)
        )),
        ProviderError::StaleAnchor => CoreError::Network(format!(
            "{what}: your monitoring node is more than {ANCHOR_MAX_LAG} blocks behind {}; its answers are too old to use",
            crate::network::rpc_origin(&network.rpc_url)
        )),
        ProviderError::Header(e) => CoreError::Rejected(format!("{what}: the node sent a block header that does not check out ({e})")),
        ProviderError::Proof(e) => CoreError::Rejected(format!("{what}: the node sent a state proof that does not check out ({e})")),
        other => {
            let text = format!("could not verify {what}: {other}");
            match other.class() {
                quai_sdk::ErrorClass::Stale | quai_sdk::ErrorClass::Transient => CoreError::Network(text),
                quai_sdk::ErrorClass::NetworkMismatch => CoreError::Rejected(text),
                _ => CoreError::Invalid(text),
            }
        }
    }
}

/// Where a node keeps its last anchor.
///
/// Every [`Node`] reading through the same monitoring node with the same witness shares one: the
/// wallet worker, the signing lane and a review's data context are separate sessions, and the
/// anchor the worker keeps warm is only useful if the review sees it. It is public chain data.
pub struct AnchorSlot {
    last: std::sync::Mutex<Option<(Anchored, Instant)>>,
    refreshing: std::sync::atomic::AtomicBool,
}

impl Default for AnchorSlot {
    fn default() -> Self {
        Self { last: std::sync::Mutex::new(None), refreshing: std::sync::atomic::AtomicBool::new(false) }
    }
}

impl AnchorSlot {
    pub(crate) fn get(&self, within: Duration) -> Option<Anchored> {
        let slot = self.last.lock().ok()?;
        slot.as_ref().filter(|(_, at)| at.elapsed() < within).map(|(a, _)| a.clone())
    }
    pub(crate) fn set(&self, anchored: &Anchored) {
        if let Ok(mut slot) = self.last.lock() {
            *slot = Some((anchored.clone(), Instant::now()));
        }
    }
    pub(crate) fn clear(&self) {
        if let Ok(mut slot) = self.last.lock() {
            *slot = None;
        }
    }
}

/// The slot shared by every node that reads through `serving` and is witnessed by `witness`.
pub(crate) fn shared_slot(serving: &str, witness: &str) -> std::sync::Arc<AnchorSlot> {
    static SLOTS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<AnchorSlot>>>> =
        std::sync::OnceLock::new();
    let key = format!("{serving}\n{witness}");
    let mut slots = SLOTS.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    slots.entry(key).or_default().clone()
}

/// How old the kept anchor may get before [`keep_warm`] reads the next one: early enough that a
/// review almost always finds one inside [`ANCHOR_REUSE`].
pub const ANCHOR_KEEP: Duration = Duration::from_secs(6);

/// Read the next anchor before the current one expires, so a review finds a confirmed block
/// waiting instead of asking the witness itself. One confirmation is a round trip to the network's
/// RPC; from the user's own node everything else is a millisecond, so without this the first
/// review after a pause waited on the public RPC for the confirmation alone.
///
/// Only for a node with a witness. One refresh runs at a time; a failure is left for the review to
/// meet and report.
pub async fn keep_warm(node: &Node, network: &NetworkProfile) {
    use std::sync::atomic::Ordering;
    if node.witness().is_none() || node.recent_anchor(ANCHOR_KEEP).is_some() {
        return;
    }
    let slot = node.anchor_slot();
    if slot.refreshing.swap(true, Ordering::AcqRel) {
        return;
    }
    let started = Instant::now();
    if let Ok(fresh) = read(node, network).await {
        crate::diag::timing("anchor.kept_warm", started);
        node.remember_anchor(&fresh);
    }
    slot.refreshing.store(false, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appdb::AppDb;
    use crate::data::{Trust, verify_pinned_all};
    use crate::network::PinnedContract;
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A mainnet block with its account proofs, captured by quai-sdk from rpc.quai.network.
    fn captured() -> Value {
        serde_json::from_str(include_str!("fixtures/state-proofs-mainnet.json")).unwrap()
    }
    /// Another real mainnet header (#10,259,520): its fields hash to its own `headerHash`.
    fn other_block() -> Value {
        serde_json::from_str(include_str!("fixtures/header-mainnet-10259520.json")).unwrap()
    }
    const WQUAI: &str = "0x006C3e2AaAE5DB1bCd11A1a097cE572312EADdBB";

    #[derive(Clone, Copy, PartialEq)]
    enum Kind {
        /// Serves the captured block at whatever height it is asked for.
        Honest,
        /// Serves another real block as the captured one's height: a header that recomputes, at a
        /// height where the chain holds a different one.
        Forger,
        /// Honest, but its head is far past the captured block.
        Ahead,
        /// Its header carries a field this SDK version does not know.
        Upgraded,
    }

    /// A JSON-RPC node on a loopback port that logs every call it answers.
    async fn serve(kind: Kind) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let log = Arc::new(Mutex::new(Vec::new()));
        let seen = log.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let seen = seen.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 8192];
                    let body = loop {
                        let Ok(n) = socket.read(&mut chunk).await else { return };
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                        let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
                        let length = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                            .unwrap_or(0);
                        if buf.len() >= end + 4 + length {
                            break buf[end + 4..end + 4 + length].to_vec();
                        }
                    };
                    let request: Value = serde_json::from_slice(&body).unwrap();
                    seen.lock().unwrap().push(request.to_string());
                    let answer = match &request {
                        Value::Array(calls) => Value::Array(calls.iter().map(|c| answer(kind, c)).collect()),
                        call => answer(kind, call),
                    };
                    let out = answer.to_string();
                    let head = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        out.len()
                    );
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(out.as_bytes()).await;
                });
            }
        });
        (url, log)
    }

    fn answer(kind: Kind, call: &Value) -> Value {
        let fixture = captured();
        let number = fixture["header"]["woHeader"]["number"].as_str().unwrap().to_string();
        let relabel = |mut header: Value, at: &str| {
            header["woHeader"]["number"] = json!(at);
            header
        };
        let params = &call["params"];
        let result = match call["method"].as_str().unwrap() {
            "quai_chainId" => json!("0x9"),
            "quai_getHeaderByNumber" if params[0] == "0x0" => fixture["genesis"].clone(),
            "quai_getHeaderByNumber" => {
                let asked = params[0].as_str().unwrap();
                let at = if asked == "latest" { number.as_str() } else { asked };
                match kind {
                    Kind::Forger => relabel(other_block(), at),
                    Kind::Ahead if asked == "latest" => {
                        let ahead = u64::from_str_radix(number.trim_start_matches("0x"), 16).unwrap() + 20;
                        relabel(fixture["header"].clone(), &format!("{ahead:#x}"))
                    }
                    Kind::Upgraded => {
                        let mut header = relabel(fixture["header"].clone(), at);
                        header["someNewSealField"] = json!("0x01");
                        header
                    }
                    _ => relabel(fixture["header"].clone(), at),
                }
            }
            "quai_getProof" => {
                let address = params[0].as_str().unwrap().to_lowercase();
                let mut proof = fixture["proofs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|p| p["result"]["address"].as_str().unwrap().to_lowercase() == address)
                    .expect("a captured proof for this address")["result"]
                    .clone();
                if params[1].as_array().is_none_or(Vec::is_empty) {
                    proof["storageProof"] = json!([]);
                }
                proof
            }
            // The runtime the fallback test pins: keccak256(0x6000).
            "quai_getCode" => json!("0x6000"),
            other => panic!("unexpected {other}"),
        };
        json!({"jsonrpc": "2.0", "id": call["id"], "result": result})
    }

    fn network(url: &str) -> NetworkProfile {
        let mainnet = NetworkProfile::builtins().into_iter().find(|n| n.id == "mainnet").unwrap();
        NetworkProfile { rpc_url: url.into(), monitor: None, ..mainnet }
    }

    fn wquai_pin() -> PinnedContract {
        let code = captured()["proofs"][0]["result"]["codeHash"].as_str().unwrap().to_string();
        PinnedContract { address: WQUAI.into(), code_hash: Some(code) }
    }

    async fn check(
        serving: Kind,
        witness: Option<Kind>,
        pin: &PinnedContract,
    ) -> (crate::error::Result<Vec<QuaiSdkAddress>>, Node, Vec<String>) {
        let (url, _) = serve(serving).await;
        let net = network(&url);
        let mut node = net.node().unwrap();
        let mut witness_log = Arc::default();
        if let Some(kind) = witness {
            let (witness_url, log) = serve(kind).await;
            witness_log = log;
            node = node.with_witness(network(&witness_url).node().unwrap());
        }
        let app = AppDb::memory().unwrap();
        let result = verify_pinned_all(&app, &node, &net, &[(pin, "WQUAI")], Trust::FirstHand).await;
        let log = witness_log.lock().unwrap().clone();
        (result, node, log)
    }
    type QuaiSdkAddress = quai_sdk::QuaiAddress;

    #[tokio::test]
    async fn a_pin_is_proven_at_a_block_the_witness_confirms_without_learning_the_address() {
        let (result, node, witness_log) = check(Kind::Honest, Some(Kind::Honest), &wquai_pin()).await;
        assert_eq!(result.unwrap(), vec![WQUAI.parse::<QuaiSdkAddress>().unwrap()]);
        let anchored = node.recent_anchor(ANCHOR_REUSE).expect("the anchor is kept for the review's other checks");
        assert_eq!(anchored.confirmation, Confirmation::Witnessed);
        assert_eq!(wquai_pin().trust_label_on(&node), "✓ pinned bytecode, confirmed by two nodes", "the review says so");
        let asked = witness_log.join("\n").to_lowercase();
        assert!(asked.contains("quai_getheaderbynumber"), "the witness was asked for the header");
        assert!(!asked.contains(&WQUAI.to_lowercase()[2..]), "and never told what is being proven: {asked}");
    }

    #[tokio::test]
    async fn without_a_witness_a_pin_is_proven_against_the_node_alone() {
        let (result, node, _) = check(Kind::Honest, None, &wquai_pin()).await;
        assert!(result.is_ok());
        assert_eq!(node.recent_anchor(ANCHOR_REUSE).unwrap().confirmation, Confirmation::NodeOnly);
        assert_eq!(wquai_pin().trust_label_on(&node), "✓ pinned bytecode", "one node's word claims no more");
    }

    /// A monitoring node that serves another real block at the height: the header hashes, every
    /// SDK check on the node alone passes, and only the witness can tell. The review is refused.
    #[tokio::test]
    async fn a_monitor_disputed_by_the_witness_is_refused() {
        let (result, _, _) = check(Kind::Forger, Some(Kind::Honest), &wquai_pin()).await;
        match result {
            Err(CoreError::Rejected(text)) => assert!(text.contains("disagree"), "{text}"),
            other => panic!("a disputed anchor was accepted: {other:?}"),
        }
    }

    /// A monitor whose block is far behind the witness's head proves an old state, which may no
    /// longer hold (code replaced, an allowance revoked).
    #[tokio::test]
    async fn a_block_far_behind_the_witness_is_too_old_to_use() {
        let (result, _, _) = check(Kind::Honest, Some(Kind::Ahead), &wquai_pin()).await;
        match result {
            Err(CoreError::Network(text)) => assert!(text.contains("behind"), "{text}"),
            other => panic!("a stale anchor was accepted: {other:?}"),
        }
    }

    /// A pin that does not match is refused whatever the anchor.
    #[tokio::test]
    async fn a_proven_code_hash_that_differs_from_the_pin_is_refused() {
        let pin = PinnedContract { address: WQUAI.into(), code_hash: Some(format!("0x{}", "11".repeat(32))) };
        let (result, _, _) = check(Kind::Honest, Some(Kind::Honest), &pin).await;
        assert!(matches!(result, Err(CoreError::Rejected(text)) if text.contains("does not match its pinned bytecode")));
    }

    /// After a go-quai upgrade adds a header field this SDK cannot hash, the pin falls back to
    /// downloading and hashing the runtime rather than refusing every review.
    #[tokio::test]
    async fn an_unknown_header_field_falls_back_to_the_bytecode_check() {
        let code = format!("0x{}", hex::encode(quai_sdk::crypto::keccak256(&[0x60, 0x00])));
        let pin = PinnedContract { address: WQUAI.into(), code_hash: Some(code) };
        let (result, _, _) = check(Kind::Upgraded, None, &pin).await;
        assert!(result.is_ok(), "the bytecode check answered: {result:?}");
    }

    /// The worker keeps an anchor warm, and the signing lane's review, a separate session with its
    /// own node, finds it there: nodes reading through the same monitor and witness share it.
    #[tokio::test]
    async fn an_anchor_kept_warm_by_one_session_serves_another() {
        let (monitor_url, _) = serve(Kind::Honest).await;
        let (rpc_url, rpc_log) = serve(Kind::Honest).await;
        let net = network(&monitor_url);
        let worker = net.node().unwrap().with_witness(network(&rpc_url).node().unwrap());
        let lane = net.node().unwrap().with_witness(network(&rpc_url).node().unwrap());
        keep_warm(&worker, &net).await;
        assert_eq!(lane.anchor_confirmation(), Some(Confirmation::Witnessed), "the lane sees the worker's anchor");
        let asked = rpc_log.lock().unwrap().len();
        let app = AppDb::memory().unwrap();
        verify_pinned_all(&app, &lane, &net, &[(&wquai_pin(), "WQUAI")], Trust::FirstHand).await.unwrap();
        assert_eq!(rpc_log.lock().unwrap().len(), asked, "the review asked the witness nothing more");
        // Fresh enough: a second keep_warm right away reads nothing.
        keep_warm(&worker, &net).await;
        assert_eq!(rpc_log.lock().unwrap().len(), asked);
        // A node without a witness is never kept warm: it has nothing to confirm with.
        let (alone_url, alone_log) = serve(Kind::Honest).await;
        keep_warm(&network(&alone_url).node().unwrap(), &net).await;
        assert!(alone_log.lock().unwrap().is_empty());
    }
}
