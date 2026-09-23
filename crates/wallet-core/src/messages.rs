//! On-chain messages: a public board read from the `Messages` contract's events.
//!
//! The contract is its own project; this is only its client. It stores nothing, so a channel
//! is one topic-filtered
//! `quai_getLogs` on the node — the same request shape the DEX flow uses. What the wallet needs
//! from the contract is fixed here: the `Message` event's signature, the `post` call, and the
//! address and runtime hash pinned in the network profile. Nothing here signs: posting goes
//! through the usual review in `ops`.
//!
//! **Everything read here is written by strangers.** Bodies are untrusted bytes: they are
//! decoded as UTF-8, stripped of control characters and never executed, resolved or followed.

use crate::data::DataCtx;
use crate::error::{CoreError, Result};
use crate::explorer::clean_text;
use crate::registry::now;
use quai_sdk::U256;
use serde::{Deserialize, Serialize};

/// `Message(address,bytes32,uint8,bytes)`.
pub const MESSAGE_TOPIC: &str = "0x12356f1f0637771f7a456862084dfbfea65506fd40c8482922c4aae6b5e87654";

/// The contract's call interface.
pub const MESSAGES_ABI: &[&str] = &["function post(bytes32 tag, uint8 kind, bytes body)"];

/// Longest body the contract accepts, and so the longest one worth composing.
pub const MAX_BODY: usize = 1024;

/// Blocks read when the board is opened from nothing (about an hour at 5 s a block).
pub const BOARD_BLOCKS: u64 = 720;

/// Posts kept per channel.
pub const BOARD_KEEP: usize = 200;

/// How to read a body.
pub const KIND_TEXT: u8 = 0;
/// A sealed body, whose format its two parties agree on off-chain.
pub const KIND_SEALED: u8 = 1;

/// One posted message.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Post {
    /// Unix seconds; estimated from the height until the block's header is read (`timed`).
    pub at: u64,
    /// `at` came from the block header.
    pub timed: bool,
    /// Block height.
    pub block: u64,
    /// Transaction hash.
    pub tx: String,
    /// Log position in the block.
    pub index: u64,
    /// Who posted it (lowercase).
    pub from: String,
    /// The tag it was filed under (32 bytes, hex).
    pub tag: String,
    /// 0 text, 1 sealed.
    pub kind: u8,
    /// The body as written, for a sealed post the raw bytes.
    pub body: Vec<u8>,
}

impl Post {
    /// Newest first: descending block, then log position.
    pub fn position(&self) -> (u64, u64) {
        (self.block, self.index)
    }

    /// The body as display text: valid UTF-8 with control characters stripped. `None` for a
    /// sealed post or bytes that are not text — those are never guessed at.
    pub fn text(&self) -> Option<String> {
        if self.kind != KIND_TEXT {
            return None;
        }
        let text = String::from_utf8(self.body.clone()).ok()?;
        // Line breaks and tabs become spaces so a stripped body does not run its words
        // together; every other control character (escape sequences included) is dropped.
        let text: String = text.chars().map(|c| if matches!(c, '\n' | '\r' | '\t') { ' ' } else { c }).collect();
        let clean = clean_text(&text);
        (!clean.trim().is_empty()).then_some(clean)
    }
}

/// The tag of a named channel: the name in UTF-8, right-padded with zeros. A reader filters on
/// it without asking anyone for a directory, and a name longer than 32 bytes is refused rather
/// than silently truncated into somebody else's channel.
pub fn channel_tag(name: &str) -> Result<[u8; 32]> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CoreError::Invalid("a channel needs a name".into()));
    }
    let bytes = name.as_bytes();
    if bytes.len() > 32 {
        return Err(CoreError::Invalid(format!("channel names are at most 32 bytes; `{name}` is {}", bytes.len())));
    }
    let mut tag = [0u8; 32];
    tag[..bytes.len()].copy_from_slice(bytes);
    Ok(tag)
}

/// The channel name a tag came from, when it is one: trailing zeros removed, and the rest
/// readable text. Tags that are not names (a sealed message's) return `None`.
pub fn tag_name(tag: &str) -> Option<String> {
    let bytes = hex::decode(tag.trim_start_matches("0x")).ok()?;
    let end = bytes.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
    let name = String::from_utf8(bytes[..end].to_vec()).ok()?;
    let clean = clean_text(&name);
    (!clean.trim().is_empty() && clean == name).then_some(clean)
}

/// Hex of a 32-byte tag, as a log topic.
pub fn tag_topic(tag: &[u8; 32]) -> String {
    format!("0x{}", hex::encode(tag))
}

fn topic_address(topic: &str) -> String {
    let t = topic.trim_start_matches("0x");
    if t.len() == 64 { format!("0x{}", &t[24..]) } else { String::new() }
}

/// Decode one `Message` log. The body is the only unindexed argument: an ABI `bytes`, so the
/// data is a 32-byte offset, a 32-byte length and then the bytes themselves.
pub fn decode_message(topics: &[String], data_hex: &str, at: u64, block: u64, tx: &str, index: u64) -> Option<Post> {
    if !topics.first()?.eq_ignore_ascii_case(MESSAGE_TOPIC) || topics.len() < 4 {
        return None;
    }
    let data = hex::decode(data_hex.trim_start_matches("0x")).ok()?;
    let word = |i: usize| data.get(i * 32..(i + 1) * 32).map(U256::from_be_slice);
    let offset: usize = word(0)?.try_into().ok()?;
    // The offset is a byte count into `data`, and must land on a word boundary we hold.
    if !offset.is_multiple_of(32) || offset / 32 >= data.len() / 32 {
        return None;
    }
    let length: usize = word(offset / 32)?.try_into().ok()?;
    if length > MAX_BODY {
        return None;
    }
    let start = offset + 32;
    let body = data.get(start..start.checked_add(length)?)?.to_vec();
    let kind: u8 = U256::from_be_slice(&hex::decode(topics[3].trim_start_matches("0x")).ok()?).try_into().ok()?;
    Some(Post {
        at,
        timed: at > 0,
        block,
        tx: tx.to_lowercase(),
        index,
        from: topic_address(&topics[1]),
        tag: topics[2].to_lowercase(),
        kind,
        body,
    })
}

/// Posts under one tag, newest first, over the last `blocks` blocks. One `quai_getLogs`
/// against the node; block times come from the headers of the blocks that carried a post.
pub async fn channel(ctx: &DataCtx, tag: &[u8; 32], blocks: u64) -> Result<Vec<Post>> {
    posts(ctx, &hex::encode(tag), blocks, |_, _| vec![*tag]).await
}

/// A sealed conversation's posts over the last `blocks` blocks, under every tag the range can
/// hold (see [`Conversation::tags_between`]), newest first.
pub async fn conversation_posts(ctx: &DataCtx, c: &Conversation, blocks: u64) -> Result<Vec<Post>> {
    posts(ctx, &hex::encode(c.id()), blocks, |from, to| c.tags_between(from, to)).await
}

/// Posts under the tags `tags(from, to)` names for the block range being read, cached under
/// `cache_key` so a later read only asks for newer blocks.
async fn posts(ctx: &DataCtx, cache_key: &str, blocks: u64, tags: impl Fn(u64, u64) -> Vec<[u8; 32]>) -> Result<Vec<Post>> {
    use quai_sdk::provider::{LogFilter, LogRange, TopicMatch};
    let contract = ctx
        .network
        .ecosystem
        .messages
        .as_ref()
        .ok_or_else(|| CoreError::NotFound(format!("no messages contract on {}", ctx.network.name)))?;
    let key = format!("{}:board:{cache_key}", ctx.network.id);
    let mut posts: Vec<Post> = ctx.app.cache_get(&key)?.and_then(|(t, _)| serde_json::from_str(&t).ok()).unwrap_or_default();
    if ctx.cache_only {
        return Ok(posts);
    }
    let contract = crate::data::verify_pinned(&ctx.app, &ctx.node, &ctx.network, contract, "messages contract", ctx.trust).await?;
    let head = ctx.node.provider.latest_header(crate::network::ZONE).await?.ok_or_else(|| CoreError::Network("no head".into()))?;
    let head_time = crate::network::header_time(&head).unwrap_or_else(now);
    let to = head.number;
    let floor = to.saturating_sub(blocks.max(1));
    let from = posts.iter().map(|p| p.block).max().map_or(floor, |b| b.saturating_add(1)).clamp(floor, to);
    let topics = vec![
        TopicMatch::AnyOf([MESSAGE_TOPIC].iter().filter_map(|t| t.parse().ok()).collect()),
        TopicMatch::Any,
        TopicMatch::AnyOf(tags(from, to).iter().filter_map(|t| tag_topic(t).parse().ok()).collect()),
    ];
    let filter =
        LogFilter::new(crate::network::ZONE, LogRange::Inclusive { from, to }).with_addresses(vec![contract.address()]).with_topics(topics);
    let logs = ctx.node.provider.logs(&filter).await?;
    let mut times: std::collections::HashMap<u64, u64> = posts.iter().filter(|p| p.timed).map(|p| (p.block, p.at)).collect();
    times.insert(to, head_time);
    let mut fresh = Vec::new();
    for log in logs.iter().filter(|l| !l.removed) {
        let block = log.inclusion.block_number;
        let at = match times.get(&block) {
            Some(t) => *t,
            None => {
                let header = ctx.node.provider.header_at(crate::network::ZONE, block).await.ok().flatten();
                let t = header.as_ref().and_then(crate::network::header_time).unwrap_or(0);
                times.insert(block, t);
                t
            }
        };
        let topics: Vec<String> = log.topics.iter().map(|t| t.to_string()).collect();
        if let Some(p) = decode_message(&topics, &log.data.to_hex(), at, block, &log.transaction_hash.to_string(), log.log_index) {
            fresh.push(p);
        }
    }
    let known: std::collections::HashSet<(String, u64)> = posts.iter().map(|p| (p.tx.clone(), p.index)).collect();
    posts.extend(fresh.into_iter().filter(|p| !known.contains(&(p.tx.clone(), p.index))));
    posts.sort_by(|a, b| b.position().cmp(&a.position()));
    posts.truncate(BOARD_KEEP);
    if let Ok(text) = serde_json::to_string(&posts) {
        let _ = ctx.app.cache_put(&key, &text);
    }
    Ok(posts)
}

/// What a public channel looks like from outside: nobody creates one, it exists as soon as
/// somebody posts under its name.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelSummary {
    /// The channel's name.
    pub name: String,
    /// Messages seen in the window.
    pub messages: u32,
    /// Unix seconds of the newest one, estimated from the head.
    pub last_at: u64,
    /// Height of the newest message. Heights only rise, so a reader compares this with what it
    /// last saw to know there is something new — unlike `messages`, which falls as old messages
    /// age out of the window.
    pub last_block: u64,
    /// Heights of the recent messages, newest first, so a reader can count exactly how many
    /// arrived after the one it last saw.
    pub recent_blocks: Vec<u64>,
}

/// Every public channel with a message in the last `blocks` blocks, busiest first. One
/// `quai_getLogs` for text messages across all tags: sealed conversations are filed under tags
/// that are not names, so they are neither counted nor shown.
pub async fn channels(ctx: &DataCtx, blocks: u64) -> Result<Vec<ChannelSummary>> {
    use quai_sdk::provider::{LogFilter, LogRange, TopicMatch};
    let contract = ctx
        .network
        .ecosystem
        .messages
        .as_ref()
        .ok_or_else(|| CoreError::NotFound(format!("no messages contract on {}", ctx.network.name)))?;
    let contract = crate::data::verify_pinned(&ctx.app, &ctx.node, &ctx.network, contract, "messages contract", ctx.trust).await?;
    let head = ctx.node.provider.latest_header(crate::network::ZONE).await?.ok_or_else(|| CoreError::Network("no head".into()))?;
    let head_time = crate::network::header_time(&head).unwrap_or_else(now);
    let to = head.number;
    let from = to.saturating_sub(blocks.max(1));
    let kind_text = format!("0x{:0>64}", format!("{KIND_TEXT:x}"));
    let filter =
        LogFilter::new(crate::network::ZONE, LogRange::Inclusive { from, to }).with_addresses(vec![contract.address()]).with_topics(vec![
            TopicMatch::AnyOf([MESSAGE_TOPIC].iter().filter_map(|t| t.parse().ok()).collect()),
            TopicMatch::Any,
            TopicMatch::Any,
            TopicMatch::AnyOf([kind_text].iter().filter_map(|t| t.parse().ok()).collect()),
        ]);
    let logs = ctx.node.provider.logs(&filter).await?;
    let mut seen: std::collections::HashMap<String, (u32, u64, Vec<u64>)> = std::collections::HashMap::new();
    for log in logs.iter().filter(|l| !l.removed) {
        let Some(tag) = log.topics.get(2).map(|t| t.to_string()) else { continue };
        // A tag that is not a readable name is a sealed conversation, not a channel.
        let Some(name) = tag_name(&tag) else { continue };
        let block = log.inclusion.block_number;
        // Heights order them; the newest is dated from the head rather than reading every block.
        let at = head_time.saturating_sub(to.saturating_sub(block) * 5);
        let e = seen.entry(name).or_insert((0, 0, Vec::new()));
        e.0 += 1;
        e.1 = e.1.max(at);
        e.2.push(block);
    }
    let mut out: Vec<ChannelSummary> = seen
        .into_iter()
        .map(|(name, (messages, last_at, mut blocks))| {
            blocks.sort_unstable_by(|a, b| b.cmp(a));
            blocks.truncate(BOARD_KEEP);
            ChannelSummary { name, messages, last_at, last_block: blocks.first().copied().unwrap_or(0), recent_blocks: blocks }
        })
        .collect();
    out.sort_by(|a, b| b.messages.cmp(&a.messages).then(a.name.cmp(&b.name)));
    Ok(out)
}

/// The arguments of a `post` call, refused here exactly where the contract would refuse them,
/// so a message that cannot be posted never reaches a review.
pub fn post_args(tag: &[u8; 32], kind: u8, body: &[u8]) -> Result<Vec<serde_json::Value>> {
    if body.is_empty() {
        return Err(CoreError::Invalid("the message is empty".into()));
    }
    if body.len() > MAX_BODY {
        return Err(CoreError::Invalid(format!("messages are at most {MAX_BODY} bytes; this one is {}", body.len())));
    }
    Ok(vec![serde_json::json!(tag_topic(tag)), serde_json::json!(kind.to_string()), serde_json::json!(format!("0x{}", hex::encode(body)))])
}

/// The contract's interface, for preparing a `post`.
pub fn interface() -> Result<quai_sdk::abi::AbiInterface> {
    quai_sdk::abi::AbiInterface::from_human_readable(MESSAGES_ABI).map_err(|e| CoreError::Invalid(format!("messages abi: {e}")))
}

// ------------------------------------------------------------------- sealed messages

/// Version bytes of the sealed body format, so a reader never guesses at a layout. v1 bodies
/// (fixed tag, exact length) are still read; everything written now is v2.
const SEALED_V1: u8 = 1;
const SEALED_V2: u8 = 2;
/// XChaCha20-Poly1305: 24-byte nonce, 16-byte authentication tag.
const NONCE: usize = 24;
const AEAD_TAG: usize = 16;
/// v2 plaintext: a big-endian length, the text, then zeros up to a bucket.
const LEN_PREFIX: usize = 2;
/// Every v2 plaintext is padded to one of these sizes (length prefix included), or to the largest
/// body the contract takes: a sealed message's size says which bucket it fell in, not its length.
const BUCKETS: [usize; 4] = [64, 128, 256, 512];
const MAX_PADDED: usize = MAX_BODY - 1 - NONCE - AEAD_TAG;
/// The longest message that fits the contract's body once sealed.
pub const MAX_SEALED_TEXT: usize = MAX_PADDED - LEN_PREFIX;
/// Blocks a conversation's filing tag lasts. Posts in different epochs sit under unrelated tags,
/// so an observer cannot collect a conversation by its tag; one epoch is the board's read window.
pub const TAG_EPOCH_BLOCKS: u64 = BOARD_BLOCKS;

/// What two people need to read and write their conversation: the key its bodies are sealed with
/// and the tags they are filed under. Both come from one ECDH between the two payment codes'
/// notification keys, so each side computes the same from what it already has — the other
/// side's public code — and nobody else can compute either.
///
/// **This hides what was said, not who said it.** The sender's address and the time of every
/// message are on chain in the clear, and so is its size rounded up to a bucket. Tags rotate every
/// [`TAG_EPOCH_BLOCKS`], but posts from the same address still link to each other. Anyone holding
/// either side's notification key can read the whole conversation.
pub struct Conversation {
    /// The v1 tag, fixed for the conversation's life: still read, no longer written.
    legacy_tag: [u8; 32],
    key: zeroize::Zeroizing<[u8; 32]>,
    /// Keys the rotating v2 tags.
    tag_key: zeroize::Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for Conversation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Conversation({}…)", &hex::encode(self.legacy_tag)[..8])
    }
}

impl Conversation {
    /// A stable identifier for this conversation (the v1 tag), for caches and tests. Messages
    /// are filed under [`Conversation::tag_at`].
    pub fn id(&self) -> [u8; 32] {
        self.legacy_tag
    }

    /// The tag a message posted at `block` is filed under.
    pub fn tag_at(&self, block: u64) -> [u8; 32] {
        let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(b"quai-messages:v2:epoch"), self.tag_key.as_ref());
        let mut tag = [0u8; 32];
        // Expanding 32 bytes from SHA-256 cannot fail.
        let _ = hk.expand(&(block / TAG_EPOCH_BLOCKS).to_be_bytes(), &mut tag);
        tag
    }

    /// Every tag a message in `from..=to` can be under: the epochs the range touches, one before
    /// (a message filed at the head can be mined in the next epoch), and the v1 tag.
    pub fn tags_between(&self, from: u64, to: u64) -> Vec<[u8; 32]> {
        let first = (from / TAG_EPOCH_BLOCKS).saturating_sub(1);
        let mut tags: Vec<[u8; 32]> = (first..=to / TAG_EPOCH_BLOCKS).map(|e| self.tag_at(e * TAG_EPOCH_BLOCKS)).collect();
        tags.push(self.legacy_tag);
        tags
    }
}

/// Derive the conversation between my payment account and a peer's public payment code.
pub fn conversation(mine: &quai_sdk::payments::PrivatePaymentCode, theirs: &quai_sdk::payments::PaymentCode) -> Result<Conversation> {
    let secret = mine.notification_key().map_err(|_| CoreError::Invalid("cannot derive this wallet's notification key".into()))?;
    // ECDH is symmetric, so both sides reach the same secret from the other's public code.
    let shared = secret.ecdh_shared_x(&theirs.notification_public_key());
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(b"quai-messages:v1"), shared.as_bytes());
    let mut legacy_tag = [0u8; 32];
    let mut key = zeroize::Zeroizing::new([0u8; 32]);
    let mut tag_key = zeroize::Zeroizing::new([0u8; 32]);
    hk.expand(b"tag", &mut legacy_tag).map_err(|_| CoreError::Invalid("tag derivation".into()))?;
    hk.expand(b"key", key.as_mut()).map_err(|_| CoreError::Invalid("key derivation".into()))?;
    hk.expand(b"tag-key:v2", tag_key.as_mut()).map_err(|_| CoreError::Invalid("tag key derivation".into()))?;
    Ok(Conversation { legacy_tag, key, tag_key })
}

/// The padded size for a plaintext of `len` bytes (length prefix included).
fn bucket(len: usize) -> usize {
    BUCKETS.iter().copied().find(|b| *b >= len).unwrap_or(MAX_PADDED)
}

/// Seal a message posted at `block` (the head when it is prepared): returns the tag to file it
/// under and the body `2 || nonce || ciphertext || mac`. The plaintext is length-prefixed and
/// padded to a bucket, and the tag is authenticated with it, so a body cannot be moved to another
/// conversation or epoch.
pub fn seal(c: &Conversation, text: &str, block: u64) -> Result<([u8; 32], Vec<u8>)> {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return Err(CoreError::Invalid("the message is empty".into()));
    }
    if bytes.len() > MAX_SEALED_TEXT {
        return Err(CoreError::Invalid(format!("a sealed message is at most {MAX_SEALED_TEXT} bytes; this one is {}", bytes.len())));
    }
    let mut plain = zeroize::Zeroizing::new(vec![0u8; bucket(LEN_PREFIX + bytes.len())]);
    plain[..LEN_PREFIX].copy_from_slice(&(bytes.len() as u16).to_be_bytes());
    plain[LEN_PREFIX..LEN_PREFIX + bytes.len()].copy_from_slice(bytes);
    let tag = c.tag_at(block);
    Ok((tag, seal_with(c, SEALED_V2, &tag, &mut plain)?))
}

/// Encrypt `plain` in place under the conversation key with `aad`, and frame it.
fn seal_with(c: &Conversation, version: u8, aad: &[u8; 32], plain: &mut [u8]) -> Result<Vec<u8>> {
    use chacha20poly1305::{AeadInOut, KeyInit, XChaCha20Poly1305, XNonce};
    let mut nonce = [0u8; NONCE];
    quai_sdk::crypto::fill_random(&mut nonce).map_err(|_| CoreError::Invalid("no randomness for the nonce".into()))?;
    let cipher = XChaCha20Poly1305::new_from_slice(c.key.as_ref()).map_err(|_| CoreError::Invalid("sealing key".into()))?;
    let mac =
        cipher.encrypt_inout_detached(&XNonce::from(nonce), aad, plain.into()).map_err(|_| CoreError::Invalid("sealing failed".into()))?;
    let mut body = Vec::with_capacity(1 + NONCE + plain.len() + AEAD_TAG);
    body.push(version);
    body.extend_from_slice(&nonce);
    body.extend_from_slice(plain);
    body.extend_from_slice(&mac);
    Ok(body)
}

/// Open a sealed body filed under `filed_under` (the post's tag, hex), or `None` when it was not
/// written for this conversation and tag, was tampered with, or is not a layout this version
/// knows. The text is sanitized like any other message: it was written by someone else, even if
/// only one other person could have written it.
pub fn open(c: &Conversation, body: &[u8], filed_under: &str) -> Option<String> {
    use chacha20poly1305::{AeadInOut, KeyInit, Tag, XChaCha20Poly1305, XNonce};
    if body.len() < 1 + NONCE + AEAD_TAG {
        return None;
    }
    let filed: [u8; 32] = hex::decode(filed_under.trim_start_matches("0x")).ok()?.try_into().ok()?;
    // v1 was only ever filed under the fixed tag; v2 authenticates whichever tag it was filed under.
    let aad = match body[0] {
        SEALED_V1 if filed == c.legacy_tag => c.legacy_tag,
        SEALED_V2 => filed,
        _ => return None,
    };
    let (nonce, rest) = body[1..].split_at(NONCE);
    let (sealed, mac) = rest.split_at(rest.len() - AEAD_TAG);
    let cipher = XChaCha20Poly1305::new_from_slice(c.key.as_ref()).ok()?;
    let nonce: [u8; NONCE] = nonce.try_into().ok()?;
    let mac: [u8; AEAD_TAG] = mac.try_into().ok()?;
    let mut plain = zeroize::Zeroizing::new(sealed.to_vec());
    cipher.decrypt_inout_detached(&XNonce::from(nonce), &aad, (&mut plain[..]).into(), &Tag::from(mac)).ok()?;
    let text = if body[0] == SEALED_V1 {
        plain.as_slice()
    } else {
        let len = usize::from(u16::from_be_bytes(plain.get(..LEN_PREFIX)?.try_into().ok()?));
        plain.get(LEN_PREFIX..LEN_PREFIX + len)?
    };
    let text = String::from_utf8(text.to_vec()).ok()?;
    let text: String = text.chars().map(|ch| if matches!(ch, '\n' | '\r' | '\t') { ' ' } else { ch }).collect();
    let clean = clean_text(&text);
    (!clean.trim().is_empty()).then_some(clean)
}

impl crate::session::Session {
    /// Messages that arrived in the followed public channels since this wallet last looked,
    /// recording where the board stands as it goes. The first look at a channel announces
    /// nothing: starting the daemon is not news, what comes after it is.
    ///
    /// Only public channels. Reading a sealed conversation needs this wallet's payment key, and
    /// this path deliberately holds none, so the daemon can run without ever unlocking.
    pub async fn track_board(&self, follows: &[String]) -> Result<Vec<(String, u32)>> {
        if follows.is_empty() || self.network.ecosystem.messages.is_none() {
            return Ok(Vec::new());
        }
        let ctx = self.data_ctx()?;
        let found = channels(&ctx, BOARD_BLOCKS).await?;
        let mut news = Vec::new();
        for name in follows {
            let Some(channel) = found.iter().find(|c| &c.name == name) else { continue };
            let key = format!("board_seen:{}:{name}", self.network.id);
            match self.app.kv(&key)?.and_then(|v| v.parse::<u64>().ok()) {
                // Heights only rise, so this is the mark to compare against next time.
                None => {
                    self.app.set_kv(&key, &channel.last_block.to_string())?;
                }
                Some(seen) => {
                    let arrived = channel.recent_blocks.iter().filter(|b| **b > seen).count() as u32;
                    if arrived > 0 {
                        self.app.set_kv(&key, &channel.last_block.to_string())?;
                        news.push((name.clone(), arrived));
                    }
                }
            }
        }
        Ok(news)
    }
}

/// A conversation's posts, oldest first, as the lines the wallet shows. `mine` are this wallet's
/// accounts and `known` the ones on record for the contact, all lowercase.
///
/// A sealed body carries a fresh random nonce, so the same bytes twice is a copy: someone
/// reposting a message lifted off the board, perhaps from a lookalike address. The first post is
/// the real one, and later copies are dropped. A body that opens proves which conversation it
/// belongs to, not who posted it, so a post from an account not on record is only pointed out.
pub fn sealed_lines(c: &Conversation, posts: &[Post], mine: &[String], known: &[String]) -> Vec<crate::ops::SealedLine> {
    let mut seen = std::collections::HashSet::new();
    let mut lines = Vec::with_capacity(posts.len());
    for p in posts {
        if !seen.insert(p.body.as_slice()) {
            continue;
        }
        let text = open(c, &p.body, &p.tag);
        let from = p.from.to_lowercase();
        let mine = mine.contains(&from);
        let new_address = !mine && text.is_some() && !known.is_empty() && !known.contains(&from);
        lines.push(crate::ops::SealedLine { at: p.at, from: p.from.clone(), mine, text, new_address });
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(hex_text: &str) -> String {
        format!("0x{:0>64}", hex_text)
    }

    /// The topic the board is read by is the hash of the event the contract declares: if one
    /// side ever changes the signature, this is what catches it.
    fn payment_account(byte: u8) -> quai_sdk::payments::PrivatePaymentCode {
        let seed = [byte; 32];
        quai_sdk::payments::PrivatePaymentCode::from_seed(&seed, 0).unwrap()
    }

    /// A body sealed the v1 way (fixed tag, no padding), as older versions wrote them.
    fn seal_v1(c: &Conversation, text: &str) -> Vec<u8> {
        let mut plain = text.as_bytes().to_vec();
        seal_with(c, SEALED_V1, &c.legacy_tag, &mut plain).unwrap()
    }

    const BLOCK: u64 = 9_700_000;

    /// The property the whole scheme rests on: each side derives the same tags and key from the
    /// other's public code alone, and a third party derives neither.
    #[test]
    fn both_sides_reach_the_same_conversation_and_nobody_else_does() {
        let (alice, bob, eve) = (payment_account(1), payment_account(2), payment_account(3));
        let a_to_b = conversation(&alice, bob.public_code()).unwrap();
        let b_to_a = conversation(&bob, alice.public_code()).unwrap();
        assert_eq!(a_to_b.tag_at(BLOCK), b_to_a.tag_at(BLOCK), "the same conversation from either side");
        // What Alice seals, Bob opens.
        let (tag, sealed) = seal(&a_to_b, "the launch is on tuesday", BLOCK).unwrap();
        assert_eq!(open(&b_to_a, &sealed, &hex::encode(tag)).as_deref(), Some("the launch is on tuesday"));
        // Eve holds neither key: a different conversation, and the body stays shut.
        let e_to_a = conversation(&eve, alice.public_code()).unwrap();
        assert_ne!(e_to_a.tag_at(BLOCK), a_to_b.tag_at(BLOCK), "Eve cannot even find the conversation");
        assert_eq!(open(&e_to_a, &sealed, &hex::encode(tag)), None, "nor read it");
        // Alice's own conversation with Eve is a different one again.
        let a_to_e = conversation(&alice, eve.public_code()).unwrap();
        assert_eq!(a_to_e.tag_at(BLOCK), e_to_a.tag_at(BLOCK));
        assert_ne!(a_to_e.tag_at(BLOCK), a_to_b.tag_at(BLOCK));
    }

    /// Tags rotate by epoch, so posts in different epochs cannot be gathered by their tag, while a
    /// reader still asks for every tag its window can hold — including the epoch before, and the
    /// v1 tag older messages used.
    #[test]
    fn tags_rotate_and_a_reader_still_finds_every_message() {
        let (alice, bob) = (payment_account(1), payment_account(2));
        let ab = conversation(&alice, bob.public_code()).unwrap();
        let epoch = BLOCK - BLOCK % TAG_EPOCH_BLOCKS;
        assert_eq!(ab.tag_at(epoch), ab.tag_at(epoch + TAG_EPOCH_BLOCKS - 1), "one tag within an epoch");
        assert_ne!(ab.tag_at(epoch), ab.tag_at(epoch + TAG_EPOCH_BLOCKS), "a new one in the next");
        assert!(![ab.tag_at(epoch), ab.tag_at(epoch + TAG_EPOCH_BLOCKS)].contains(&ab.id()), "neither is the v1 tag");
        // Filed at the head, mined a block later in the next epoch, read with a window that starts
        // after the epoch it was filed in: still found.
        let filed = ab.tag_at(epoch + TAG_EPOCH_BLOCKS - 1);
        let wanted = ab.tags_between(epoch + TAG_EPOCH_BLOCKS + 5, epoch + 2 * TAG_EPOCH_BLOCKS);
        assert!(wanted.contains(&filed) && wanted.contains(&ab.id()));
    }

    /// Every v2 body of a bucket is the same size whatever the text's length, so its size says
    /// only which bucket it fell in; the length inside is authenticated with the rest.
    #[test]
    fn sealed_bodies_are_padded_to_buckets() {
        let (alice, bob) = (payment_account(1), payment_account(2));
        let (ab, ba) = (conversation(&alice, bob.public_code()).unwrap(), conversation(&bob, alice.public_code()).unwrap());
        let size = |text: &str| seal(&ab, text, BLOCK).unwrap().1.len();
        assert_eq!(size("hi"), size(&"x".repeat(60)), "same bucket, same size");
        assert!(size(&"x".repeat(63)) > size("hi"), "past the bucket, the next one");
        assert_eq!(size(&"x".repeat(600)), size(&"x".repeat(MAX_SEALED_TEXT)), "the largest bucket is the whole body");
        let (tag, body) = seal(&ab, "short", BLOCK).unwrap();
        assert_eq!(open(&ba, &body, &hex::encode(tag)).as_deref(), Some("short"), "padding is not part of the text");
    }

    /// Messages written by older versions still open, under the tag they were filed with only.
    #[test]
    fn v1_messages_still_open() {
        let (alice, bob) = (payment_account(1), payment_account(2));
        let (ab, ba) = (conversation(&alice, bob.public_code()).unwrap(), conversation(&bob, alice.public_code()).unwrap());
        let old = seal_v1(&ab, "from before the upgrade");
        assert_eq!(open(&ba, &old, &hex::encode(ab.id())).as_deref(), Some("from before the upgrade"));
        assert_eq!(open(&ba, &old, &hex::encode(ab.tag_at(BLOCK))), None, "a v1 body under another tag is refused");
    }

    /// A sealed body is authenticated: a changed byte, a replay into another conversation or
    /// epoch, or a layout this version does not know all fail shut rather than producing something
    /// plausible.
    #[test]
    fn a_tampered_or_replayed_body_does_not_open() {
        let (alice, bob, eve) = (payment_account(1), payment_account(2), payment_account(3));
        let ab = conversation(&alice, bob.public_code()).unwrap();
        let (tag, sealed) = seal(&ab, "meet at six", BLOCK).unwrap();
        let filed = hex::encode(tag);
        assert_eq!(open(&ab, &sealed, &filed).as_deref(), Some("meet at six"));
        for i in [0, 1, 25, sealed.len() - 1] {
            let mut bad = sealed.clone();
            bad[i] ^= 0x01;
            assert_eq!(open(&ab, &bad, &filed), None, "byte {i} changed and it still opened");
        }
        // The tag is authenticated: the same body filed in another epoch or conversation is
        // refused, even by someone who holds that conversation's key.
        assert_eq!(open(&ab, &sealed, &hex::encode(ab.tag_at(BLOCK + TAG_EPOCH_BLOCKS))), None, "moved to another epoch");
        let ae = conversation(&alice, eve.public_code()).unwrap();
        assert_eq!(open(&ae, &sealed, &filed), None, "not written for this conversation");
        // Truncation, a malformed tag and a version this build does not know.
        assert_eq!(open(&ab, &sealed[..sealed.len() - 1], &filed), None);
        assert_eq!(open(&ab, &[], &filed), None);
        assert_eq!(open(&ab, &sealed, "not hex"), None);
        let mut future = sealed.clone();
        future[0] = 99;
        assert_eq!(open(&ab, &future, &filed), None, "an unknown layout is never guessed at");
    }

    /// A sealed message fits the contract's body, and what comes out is treated as hostile text
    /// exactly like a public post — being the only other party does not make it trustworthy.
    #[test]
    fn a_sealed_message_fits_the_contract_and_is_still_sanitized() {
        let (alice, bob) = (payment_account(1), payment_account(2));
        let ab = conversation(&alice, bob.public_code()).unwrap();
        let ba = conversation(&bob, alice.public_code()).unwrap();
        assert!(seal(&ab, "", BLOCK).is_err(), "an empty message is refused");
        assert!(seal(&ab, &"x".repeat(MAX_SEALED_TEXT + 1), BLOCK).is_err(), "and one too long to post");
        let (tag, longest) = seal(&ab, &"x".repeat(MAX_SEALED_TEXT), BLOCK).unwrap();
        assert_eq!(longest.len(), MAX_BODY, "the longest sealed message fills the body exactly");
        assert!(post_args(&tag, KIND_SEALED, &longest).is_ok());
        let (tag, nasty) = seal(&ab, "clear\x1b[2Jthis\x07 and\na line", BLOCK).unwrap();
        let text = open(&ba, &nasty, &hex::encode(tag)).unwrap();
        assert!(!text.contains('\x1b') && !text.contains('\x07'), "escape sequences are stripped: {text:?}");
        assert!(text.contains("and a line"), "line breaks become spaces: {text:?}");
    }

    /// The rule the daemon announces by, without a chain: the first look at a channel records
    /// where it stands, later looks count only what rose above that mark, and a mark that has
    /// not moved is not news. `messages` is deliberately not used — it falls as old messages age
    /// out of the window, which would read as arrivals.
    #[test]
    fn a_channel_is_news_only_above_the_mark_last_recorded() {
        let app = crate::appdb::AppDb::memory().unwrap();
        let key = "board_seen:local:general";
        let summary = |blocks: Vec<u64>| ChannelSummary {
            name: "general".into(),
            messages: blocks.len() as u32,
            last_at: 0,
            last_block: blocks.first().copied().unwrap_or(0),
            recent_blocks: blocks,
        };
        // The same arithmetic `track_board` runs, against the store it keeps it in.
        let arrived = |app: &crate::appdb::AppDb, c: &ChannelSummary| -> u32 {
            match app.kv(key).unwrap().and_then(|v| v.parse::<u64>().ok()) {
                None => {
                    app.set_kv(key, &c.last_block.to_string()).unwrap();
                    0
                }
                Some(seen) => {
                    let n = c.recent_blocks.iter().filter(|b| **b > seen).count() as u32;
                    if n > 0 {
                        app.set_kv(key, &c.last_block.to_string()).unwrap();
                    }
                    n
                }
            }
        };
        assert_eq!(arrived(&app, &summary(vec![100, 99])), 0, "the first look is not news");
        assert_eq!(app.kv(key).unwrap().as_deref(), Some("100"));
        assert_eq!(arrived(&app, &summary(vec![102, 101, 100, 99])), 2, "two rose above the mark");
        assert_eq!(arrived(&app, &summary(vec![102, 101, 100, 99])), 0, "the same board again is not news");
        // Old messages ageing out of the window lowers the count without anything arriving.
        assert_eq!(arrived(&app, &summary(vec![102, 101])), 0, "a shrinking window is not news");
    }

    #[test]
    fn the_message_topic_is_the_events_signature() {
        let signature = b"Message(address,bytes32,uint8,bytes)";
        assert_eq!(MESSAGE_TOPIC, format!("0x{}", hex::encode(quai_sdk::crypto::keccak256(signature))));
    }

    #[test]
    fn channel_names_round_trip_and_refuse_to_be_truncated() {
        let tag = channel_tag("quai-general").unwrap();
        assert_eq!(tag_name(&tag_topic(&tag)).as_deref(), Some("quai-general"));
        assert!(channel_tag("  ").is_err(), "a channel needs a name");
        assert!(channel_tag(&"x".repeat(33)).is_err(), "never silently truncated into another channel");
        assert_eq!(channel_tag("x".repeat(32).as_str()).unwrap().len(), 32);
        // A sealed message's tag is not a name, and is not guessed at.
        assert_eq!(tag_name(&format!("0x{}", "ab".repeat(32))), None);
    }

    #[test]
    fn a_message_log_decodes_to_its_text() {
        let body = "hello Cyprus-1".as_bytes();
        let mut data = String::new();
        data.push_str(&word("20")); // offset
        data.push_str(&word(&format!("{:x}", body.len())));
        data.push_str(&format!("{:0<64}", hex::encode(body)));
        let topics = vec![
            MESSAGE_TOPIC.to_string(),
            word("00ab1234000000000000000000000000000000ff"),
            tag_topic(&channel_tag("general").unwrap()),
            word("0"),
        ];
        let p = decode_message(&topics, &data.replace("0x", ""), 1_700_000_000, 42, "0xAA", 3).unwrap();
        assert_eq!(p.text().as_deref(), Some("hello Cyprus-1"));
        assert_eq!(p.kind, KIND_TEXT);
        assert_eq!(p.tx, "0xaa", "hashes compare lowercase");
        assert_eq!(tag_name(&p.tag).as_deref(), Some("general"));
        assert!(p.from.ends_with("00ff"), "{}", p.from);
        // A sealed body is never shown as text, whatever it happens to contain.
        let sealed = Post { kind: KIND_SEALED, ..p.clone() };
        assert_eq!(sealed.text(), None);
        // Another event's topic is not a message.
        let mut other = topics.clone();
        other[0] = word("dead");
        assert!(decode_message(&other, &data, 0, 42, "0xaa", 3).is_none());
    }

    #[test]
    fn untrusted_bodies_never_become_control_characters_or_lies() {
        let nasty = b"clear\x1b[2Jscreen\x07\nand a line";
        let p = Post {
            at: 0,
            timed: false,
            block: 1,
            tx: "0x01".into(),
            index: 0,
            from: "0x00ab".into(),
            tag: tag_topic(&channel_tag("general").unwrap()),
            kind: KIND_TEXT,
            body: nasty.to_vec(),
        };
        let text = p.text().unwrap();
        assert!(!text.contains('\x1b') && !text.contains('\x07'), "escape sequences are stripped: {text:?}");
        // Bytes that are not text are not guessed at.
        let invalid = Post { body: vec![0xff, 0xfe, 0xfd], ..p.clone() };
        assert_eq!(invalid.text(), None);
        let empty = Post { body: b"   ".to_vec(), ..p };
        assert_eq!(empty.text(), None, "a blank message has nothing to show");
    }

    #[test]
    fn a_body_the_contract_would_refuse_never_reaches_a_review() {
        let tag = channel_tag("general").unwrap();
        assert!(post_args(&tag, KIND_TEXT, b"").is_err(), "empty");
        assert!(post_args(&tag, KIND_TEXT, &vec![b'x'; MAX_BODY + 1]).is_err(), "too long");
        let args = post_args(&tag, KIND_TEXT, b"hi").unwrap();
        assert_eq!(args.len(), 3, "tag, kind and the body");
        // The interface the review prepares the call through accepts them.
        interface().unwrap();
    }

    /// A log claiming a body longer than the contract accepts, or one pointing past the data it
    /// carries, is dropped rather than trusted or panicked on.
    #[test]
    fn a_lying_log_is_dropped() {
        let topics = vec![MESSAGE_TOPIC.to_string(), word("00ab"), tag_topic(&channel_tag("general").unwrap()), word("0")];
        let huge = format!("{}{}", word("20").replace("0x", ""), word("ffff").replace("0x", ""));
        assert!(decode_message(&topics, &huge, 0, 1, "0xaa", 0).is_none(), "length past the end");
        let past = format!("{}{}", word("4000").replace("0x", ""), word("2").replace("0x", ""));
        assert!(decode_message(&topics, &past, 0, 1, "0xaa", 0).is_none(), "offset past the end");
        assert!(decode_message(&topics, "0x", 0, 1, "0xaa", 0).is_none(), "no data at all");
    }

    /// A sealed message copied onto the board from another address is dropped, not shown as a
    /// second message from the contact; and a post from an account not on record is marked, never
    /// learned.
    #[test]
    fn a_replayed_sealed_body_is_dropped_and_nothing_is_learned() {
        let (alice, bob) = (payment_account(1), payment_account(2));
        let bob_reads = conversation(&bob, alice.public_code()).unwrap();
        let alice_writes = conversation(&alice, bob.public_code()).unwrap();
        let (tag, body) = seal(&alice_writes, "meet at noon", BLOCK).unwrap();
        let (tag2, body2) = seal(&alice_writes, "from my other account", BLOCK).unwrap();
        let post = |from: &str, tag: [u8; 32], body: &[u8], block: u64| Post {
            at: block,
            timed: true,
            block,
            tx: format!("0x{block}"),
            index: 0,
            from: from.into(),
            tag: hex::encode(tag),
            kind: KIND_SEALED,
            body: body.to_vec(),
        };
        let alice_addr = "0x00a1000000000000000000000000000000000001";
        let lookalike = "0x00a1000000000000000000000000000000ffff01";
        let alice_second = "0x00a2000000000000000000000000000000000002";
        let bob_addr = "0x00b0000000000000000000000000000000000000";
        let posts =
            vec![post(alice_addr, tag, &body, BLOCK), post(lookalike, tag, &body, BLOCK + 5), post(alice_second, tag2, &body2, BLOCK + 9)];
        let lines = sealed_lines(&bob_reads, &posts, &[bob_addr.into()], &[alice_addr.into()]);
        assert_eq!(lines.len(), 2, "the copy is gone: {lines:?}");
        assert_eq!((lines[0].from.as_str(), lines[0].text.as_deref(), lines[0].new_address), (alice_addr, Some("meet at noon"), false));
        assert_eq!(lines[1].text.as_deref(), Some("from my other account"));
        assert!(lines[1].new_address, "an unrecorded account is pointed out");
    }
}
