//! On-chain messages: a public board read from the `Messages` contract's events.
//!
//! The contract is its own project; this is only its format. It stores nothing, so a channel
//! is one topic-filtered `quai_getLogs` on the node (the reads are wallet-core's `messages`). What the wallet needs
//! from the contract is fixed here: the `Message` event's signature, the `post` call, and the
//! address and runtime hash pinned in the network profile. Nothing here signs: posting goes
//! through the usual review in `ops`.
//!
//! **Everything read here is written by strangers.** Bodies are untrusted bytes: they are
//! decoded as UTF-8, stripped of control characters and never executed, resolved or followed.

use quai_model::error::{CoreError, Result};
use quai_model::text::clean_text;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn word(hex_text: &str) -> String {
        format!("0x{:0>64}", hex_text)
    }

    /// The rule the daemon announces by, without a chain: the first look at a channel records
    /// where it stands, later looks count only what rose above that mark, and a mark that has
    /// not moved is not news. `messages` is deliberately not used — it falls as old messages age
    /// out of the window, which would read as arrivals.
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
}
