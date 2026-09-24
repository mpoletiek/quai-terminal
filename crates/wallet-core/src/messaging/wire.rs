//! The v3 byte formats: key announcements (kind 2) and sealed direct messages (kind 3).
//!
//! Everything here is pure: bytes in, bytes out, no network and no storage, so another
//! implementation can be checked against it byte for byte (`docs/MESSAGING_V3.md` and the test
//! vectors in `fixtures/messaging_v3_vectors.json`).
//!
//! **Announcement** (138 bytes), posted from the messaging account under [`keys_tag`]:
//! `version | suite | week (4) | sequence (4) | identity key (32) | weekly key (32) | signature (64)`.
//! The Ed25519 signature covers a label, the chain, the contract, the posting address and the
//! first 74 bytes, so an announcement cannot be replayed on another chain or under another address.
//!
//! **Direct message**, posted from the sender's messaging account under 32 random bytes:
//! `version | suite | sender weekly key (32) | enc (32) | ciphertext`. The ciphertext is one HPKE
//! seal in auth mode, from the sender's weekly key to the recipient's, over a padded plaintext
//! `content type | length (2) | content | zeros`. The associated data binds the chain, the
//! contract, both addresses, the tag and the header, so a body copied by another address, sent
//! back the other way, moved to another chain or re-filed under another tag does not open.
//!
//! Nothing in a DM's header names the recipient or which of their keys was used: the recipient
//! tries each key it holds. The sender's weekly key is in the clear because the sender's address
//! is public anyway, and it lets a reader try a body before looking anyone up.

use hpke::{Deserializable, Kem as _, OpModeR, OpModeS, Serializable};
use zeroize::Zeroizing;

type Kem = hpke::kem::X25519HkdfSha256;
type Kdf = hpke::kdf::HkdfSha256;
type Aead = hpke::aead::ChaCha20Poly1305;

/// First byte of every v3 body.
pub const VERSION: u8 = 3;
/// DHKEM(X25519, HKDF-SHA256), HKDF-SHA256, ChaCha20-Poly1305, mode auth.
pub const SUITE: u8 = 1;
/// The contract's `kind` for a key announcement.
pub const KIND_KEYS: u8 = 2;
/// The contract's `kind` for a sealed direct message.
pub const KIND_DM: u8 = 3;
/// Longest body the contract accepts.
pub const MAX_BODY: usize = crate::messages::MAX_BODY;

const KEYS_LABEL: &[u8] = b"quai-messages:v3:keys";
const DM_INFO: &[u8] = b"quai-messages:v3:dm";
const FINGERPRINT_LABEL: &[u8] = b"quai-messages:v3:fingerprint";

/// Announcement length, and the signed prefix inside it.
pub const ANNOUNCEMENT_LEN: usize = 138;
const SIGNED_LEN: usize = 74;
/// DM header: version, suite, sender weekly key, encapsulated key.
pub const DM_HEADER: usize = 66;
const AEAD_TAG: usize = 16;
/// Plaintext prefix: content type and a big-endian length.
const CONTENT_PREFIX: usize = 3;
/// Every plaintext is padded to one of these, or to the most the body can hold.
const BUCKETS: [usize; 4] = [64, 128, 256, 512];
/// The most padded plaintext a body can carry.
pub const MAX_PADDED: usize = MAX_BODY - DM_HEADER - AEAD_TAG;
/// The longest content a DM can carry.
pub const MAX_CONTENT: usize = MAX_PADDED - CONTENT_PREFIX;

/// What a DM's content is.
pub const CONTENT_TEXT: u8 = 0x01;

/// Seconds in a week; a week number is Unix time divided by this.
pub const WEEK_SECS: u64 = 604_800;

/// The week a Unix time falls in.
pub fn week_of(unix: u64) -> u32 {
    u32::try_from(unix / WEEK_SECS).unwrap_or(u32::MAX)
}

/// The tag every announcement is filed under: `keccak256("quai-messages:v3:keys")`.
pub fn keys_tag() -> [u8; 32] {
    quai_sdk::crypto::keccak256(KEYS_LABEL)
}

/// Where a body lives: the chain and the contract. Bound into every signature and seal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Context {
    pub chain_id: u64,
    pub contract: [u8; 20],
}

/// A 20-byte address from its hex form.
pub fn address_bytes(address: &str) -> Option<[u8; 20]> {
    hex::decode(address.trim().trim_start_matches("0x")).ok()?.try_into().ok()
}

// ---------------------------------------------------------------------------- keys

/// An X25519 weekly key pair, raw. The secret is wiped when dropped.
pub struct WeeklySecret {
    secret: Zeroizing<[u8; 32]>,
    public: [u8; 32],
}

impl WeeklySecret {
    /// A fresh key from the operating system's randomness.
    pub fn generate() -> Option<Self> {
        let mut ikm = Zeroizing::new([0u8; 32]);
        quai_sdk::crypto::fill_random(ikm.as_mut()).ok()?;
        Some(Self::derive(&ikm))
    }

    /// RFC 9180 `DeriveKeyPair` from 32 bytes of key material.
    pub fn derive(ikm: &[u8; 32]) -> Self {
        let (sk, pk) = Kem::derive_keypair(ikm);
        let mut secret = Zeroizing::new([0u8; 32]);
        secret.copy_from_slice(&sk.to_bytes());
        let mut public = [0u8; 32];
        public.copy_from_slice(&pk.to_bytes());
        Self { secret, public }
    }

    /// Rebuild from stored bytes.
    pub fn from_parts(secret: [u8; 32], public: [u8; 32]) -> Self {
        Self { secret: Zeroizing::new(secret), public }
    }

    pub fn secret_bytes(&self) -> &[u8; 32] {
        &self.secret
    }

    pub fn public(&self) -> [u8; 32] {
        self.public
    }

    fn private_key(&self) -> Option<<Kem as hpke::Kem>::PrivateKey> {
        <Kem as hpke::Kem>::PrivateKey::from_bytes(self.secret.as_ref()).ok()
    }
}

/// An Ed25519 identity key. The secret is wiped when dropped.
pub struct IdentitySecret {
    key: ed25519_dalek::SigningKey,
}

impl IdentitySecret {
    pub fn generate() -> Option<Self> {
        let mut seed = Zeroizing::new([0u8; 32]);
        quai_sdk::crypto::fill_random(seed.as_mut()).ok()?;
        Some(Self::from_seed(&seed))
    }

    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self { key: ed25519_dalek::SigningKey::from_bytes(seed) }
    }

    pub fn seed(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.key.to_bytes())
    }

    pub fn public(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }
}

/// What two people compare to know they hold each other's real key: a hash of the identity key
/// and the messaging address, as eight groups of four hex digits (128 bits).
pub fn fingerprint(identity: &[u8; 32], address: &[u8; 20]) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(FINGERPRINT_LABEL);
    h.update(address);
    h.update(identity);
    let digest = h.finalize();
    digest[..16].chunks(2).map(hex::encode).collect::<Vec<_>>().join(" ")
}

// -------------------------------------------------------------------- announcements

/// A published weekly key, signed by the identity key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Announcement {
    pub week: u32,
    pub sequence: u32,
    pub identity: [u8; 32],
    pub weekly: [u8; 32],
    pub signature: [u8; 64],
}

fn signed_message(ctx: &Context, owner: &[u8; 20], prefix: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(KEYS_LABEL.len() + 8 + 20 + 20 + SIGNED_LEN);
    m.extend_from_slice(KEYS_LABEL);
    m.extend_from_slice(&ctx.chain_id.to_be_bytes());
    m.extend_from_slice(&ctx.contract);
    m.extend_from_slice(owner);
    m.extend_from_slice(prefix);
    m
}

impl Announcement {
    /// Sign a weekly key for `owner` (the messaging account that will post it).
    pub fn sign(ctx: &Context, owner: &[u8; 20], identity: &IdentitySecret, week: u32, sequence: u32, weekly: [u8; 32]) -> Self {
        use ed25519_dalek::Signer;
        let mut a = Self { week, sequence, identity: identity.public(), weekly, signature: [0; 64] };
        let body = a.encode();
        a.signature = identity.key.sign(&signed_message(ctx, owner, &body[..SIGNED_LEN])).to_bytes();
        a
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(ANNOUNCEMENT_LEN);
        b.push(VERSION);
        b.push(SUITE);
        b.extend_from_slice(&self.week.to_be_bytes());
        b.extend_from_slice(&self.sequence.to_be_bytes());
        b.extend_from_slice(&self.identity);
        b.extend_from_slice(&self.weekly);
        b.extend_from_slice(&self.signature);
        b
    }

    /// Parse and check an announcement posted by `owner`: `None` unless the layout is exactly
    /// this version's and the signature verifies under the identity key it carries.
    pub fn verify(ctx: &Context, owner: &[u8; 20], body: &[u8]) -> Option<Self> {
        if body.len() != ANNOUNCEMENT_LEN || body[0] != VERSION || body[1] != SUITE {
            return None;
        }
        let a = Self {
            week: u32::from_be_bytes(body[2..6].try_into().ok()?),
            sequence: u32::from_be_bytes(body[6..10].try_into().ok()?),
            identity: body[10..42].try_into().ok()?,
            weekly: body[42..74].try_into().ok()?,
            signature: body[74..138].try_into().ok()?,
        };
        let key = ed25519_dalek::VerifyingKey::from_bytes(&a.identity).ok()?;
        let signature = ed25519_dalek::Signature::from_bytes(&a.signature);
        key.verify_strict(&signed_message(ctx, owner, &body[..SIGNED_LEN]), &signature).ok()?;
        // A weekly key HPKE cannot use is no key at all.
        <Kem as hpke::Kem>::PublicKey::from_bytes(&a.weekly).ok()?;
        Some(a)
    }
}

// ------------------------------------------------------------------------- messages

/// Who a DM is between, and where it is filed: everything its associated data binds.
#[derive(Clone, Copy, Debug)]
pub struct Envelope<'a> {
    pub ctx: &'a Context,
    pub sender: &'a [u8; 20],
    pub recipient: &'a [u8; 20],
    pub tag: &'a [u8; 32],
}

fn aad(e: &Envelope, header: &[u8]) -> Vec<u8> {
    let mut a = Vec::with_capacity(DM_INFO.len() + 8 + 20 + 20 + 20 + 32 + DM_HEADER);
    a.extend_from_slice(DM_INFO);
    a.extend_from_slice(&e.ctx.chain_id.to_be_bytes());
    a.extend_from_slice(&e.ctx.contract);
    a.extend_from_slice(e.sender);
    a.extend_from_slice(e.recipient);
    a.extend_from_slice(e.tag);
    a.extend_from_slice(header);
    a
}

fn bucket(len: usize) -> usize {
    BUCKETS.iter().copied().find(|b| *b >= len).unwrap_or(MAX_PADDED)
}

/// A fresh random tag to file a DM under.
pub fn random_tag() -> Option<[u8; 32]> {
    let mut tag = [0u8; 32];
    quai_sdk::crypto::fill_random(&mut tag).ok()?;
    Some(tag)
}

/// Why a DM could not be sealed.
#[derive(Debug, PartialEq, Eq)]
pub enum SealError {
    Empty,
    TooLong(usize),
    BadKey,
    Crypto,
}

/// Seal `content` from `sender_key` to the recipient's weekly public key.
pub fn seal(
    e: &Envelope,
    sender_key: &WeeklySecret,
    recipient_weekly: &[u8; 32],
    content_type: u8,
    content: &[u8],
) -> Result<Vec<u8>, SealError> {
    if content.is_empty() {
        return Err(SealError::Empty);
    }
    if content.len() > MAX_CONTENT {
        return Err(SealError::TooLong(content.len()));
    }
    let pk_r = <Kem as hpke::Kem>::PublicKey::from_bytes(recipient_weekly).map_err(|_| SealError::BadKey)?;
    let sk_s = sender_key.private_key().ok_or(SealError::BadKey)?;
    let pk_s = <Kem as hpke::Kem>::PublicKey::from_bytes(&sender_key.public).map_err(|_| SealError::BadKey)?;
    let mut plain = Zeroizing::new(vec![0u8; bucket(CONTENT_PREFIX + content.len())]);
    plain[0] = content_type;
    plain[1..3].copy_from_slice(&(content.len() as u16).to_be_bytes());
    plain[CONTENT_PREFIX..CONTENT_PREFIX + content.len()].copy_from_slice(content);
    // The encapsulated key is part of the header the associated data binds, and HPKE only hands
    // it back after sealing, so the associated data covers the fixed part of the header and HPKE
    // itself binds `enc` into the key schedule.
    let mut header = Vec::with_capacity(DM_HEADER);
    header.push(VERSION);
    header.push(SUITE);
    header.extend_from_slice(&sender_key.public);
    let (enc, ct) = hpke::single_shot_seal::<Aead, Kdf, Kem>(&OpModeS::Auth((sk_s, pk_s)), &pk_r, DM_INFO, &plain, &aad(e, &header))
        .map_err(|_| SealError::Crypto)?;
    let mut body = header;
    body.extend_from_slice(&enc.to_bytes());
    body.extend_from_slice(&ct);
    debug_assert!(body.len() <= MAX_BODY);
    Ok(body)
}

/// A DM that opened.
#[derive(Debug, PartialEq, Eq)]
pub struct Opened {
    /// The sender's weekly key; the caller must find it in an announcement from the sender's
    /// address before believing who wrote it.
    pub sender_weekly: [u8; 32],
    /// Which of the recipient's keys opened it.
    pub opened_with: [u8; 32],
    pub content_type: u8,
    pub content: Zeroizing<Vec<u8>>,
}

/// The sender's weekly key from a DM's header, without opening it.
pub fn sender_key(body: &[u8]) -> Option<[u8; 32]> {
    (body.len() > DM_HEADER && body[0] == VERSION && body[1] == SUITE).then(|| body[2..34].try_into().ok()).flatten()
}

/// Try to open a DM with each of `keys` (the recipient's weekly keys still held). `None` when it
/// is not for any of them, was tampered with, was moved, or is not a layout this version knows.
pub fn open(e: &Envelope, body: &[u8], keys: &[&WeeklySecret]) -> Option<Opened> {
    if body.len() < DM_HEADER + AEAD_TAG + BUCKETS[0] || body.len() > MAX_BODY || body[0] != VERSION || body[1] != SUITE {
        return None;
    }
    let sender_weekly: [u8; 32] = body[2..34].try_into().ok()?;
    let pk_s = <Kem as hpke::Kem>::PublicKey::from_bytes(&sender_weekly).ok()?;
    let enc = <Kem as hpke::Kem>::EncappedKey::from_bytes(&body[34..66]).ok()?;
    let aad = aad(e, &body[..34]);
    let ct = &body[DM_HEADER..];
    for key in keys {
        let Some(sk) = key.private_key() else { continue };
        let Ok(plain) = hpke::single_shot_open::<Aead, Kdf, Kem>(&OpModeR::Auth(pk_s.clone()), &sk, &enc, DM_INFO, ct, &aad) else {
            continue;
        };
        let plain = Zeroizing::new(plain);
        return unpad(&plain).map(|(content_type, content)| Opened {
            sender_weekly,
            opened_with: key.public,
            content_type,
            content: Zeroizing::new(content.to_vec()),
        });
    }
    None
}

/// Content type and content from a padded plaintext. Strict: the size must be a bucket, the
/// length must fit, and the padding must be zeros, so there is exactly one encoding of a message.
fn unpad(plain: &[u8]) -> Option<(u8, &[u8])> {
    if !(BUCKETS.contains(&plain.len()) || plain.len() == MAX_PADDED) || plain.len() < CONTENT_PREFIX {
        return None;
    }
    let len = usize::from(u16::from_be_bytes(plain[1..3].try_into().ok()?));
    if len == 0 || CONTENT_PREFIX + len > plain.len() || bucket(CONTENT_PREFIX + len) != plain.len() {
        return None;
    }
    plain[CONTENT_PREFIX + len..].iter().all(|b| *b == 0).then_some((plain[0], &plain[CONTENT_PREFIX..CONTENT_PREFIX + len]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTX: Context = Context { chain_id: 9, contract: [0x77; 20] };
    const ALICE: [u8; 20] = [0xa1; 20];
    const BOB: [u8; 20] = [0xb0; 20];
    const EVE: [u8; 20] = [0xee; 20];
    const TAG: [u8; 32] = [0x42; 32];

    fn keys() -> (WeeklySecret, WeeklySecret, WeeklySecret) {
        (WeeklySecret::derive(&[1; 32]), WeeklySecret::derive(&[2; 32]), WeeklySecret::derive(&[3; 32]))
    }

    fn env<'a>(sender: &'a [u8; 20], recipient: &'a [u8; 20], tag: &'a [u8; 32]) -> Envelope<'a> {
        Envelope { ctx: &CTX, sender, recipient, tag }
    }

    #[test]
    fn the_keys_tag_is_the_hash_of_its_label() {
        assert_eq!(hex::encode(keys_tag()), hex::encode(quai_sdk::crypto::keccak256(b"quai-messages:v3:keys")));
        assert_ne!(crate::messages::tag_name(&crate::messages::tag_topic(&keys_tag())), Some("quai-messages:v3:keys".into()));
    }

    #[test]
    fn a_message_opens_for_its_recipient_and_says_who_sent_it() {
        let (a, b, _) = keys();
        let body = seal(&env(&ALICE, &BOB, &TAG), &a, &b.public(), CONTENT_TEXT, b"the launch is on tuesday").unwrap();
        let opened = open(&env(&ALICE, &BOB, &TAG), &body, &[&b]).unwrap();
        assert_eq!(opened.content.as_slice(), b"the launch is on tuesday");
        assert_eq!((opened.content_type, opened.sender_weekly, opened.opened_with), (CONTENT_TEXT, a.public(), b.public()));
        assert_eq!(sender_key(&body), Some(a.public()));
        assert!(body.windows(8).all(|w| w != b"the laun"), "no plaintext in the body");
    }

    #[test]
    fn the_recipient_tries_every_key_it_holds() {
        let (a, b, c) = keys();
        let body = seal(&env(&ALICE, &BOB, &TAG), &a, &b.public(), CONTENT_TEXT, b"hi").unwrap();
        assert!(open(&env(&ALICE, &BOB, &TAG), &body, &[&c, &b]).is_some(), "an older key still held opens it");
        assert!(open(&env(&ALICE, &BOB, &TAG), &body, &[&c]).is_none(), "a key deleted is a message lost");
    }

    /// The attacks the associated data exists for: a copy posted from another address, a body
    /// reflected back at its sender, another chain, another contract, another tag.
    #[test]
    fn a_moved_body_does_not_open() {
        let (a, b, _) = keys();
        let body = seal(&env(&ALICE, &BOB, &TAG), &a, &b.public(), CONTENT_TEXT, b"hi").unwrap();
        assert!(open(&env(&EVE, &BOB, &TAG), &body, &[&b]).is_none(), "copied by eve");
        assert!(open(&env(&BOB, &ALICE, &TAG), &body, &[&a]).is_none(), "reflected");
        assert!(open(&env(&ALICE, &EVE, &TAG), &body, &[&b]).is_none(), "claimed for another recipient");
        assert!(open(&env(&ALICE, &BOB, &[0x43; 32]), &body, &[&b]).is_none(), "re-filed under another tag");
        let other_chain = Context { chain_id: 15000, ..CTX };
        assert!(open(&Envelope { ctx: &other_chain, sender: &ALICE, recipient: &BOB, tag: &TAG }, &body, &[&b]).is_none());
        let other_contract = Context { contract: [0x78; 20], ..CTX };
        assert!(open(&Envelope { ctx: &other_contract, sender: &ALICE, recipient: &BOB, tag: &TAG }, &body, &[&b]).is_none());
    }

    /// A different sender key in the header (someone claiming another's key) fails auth.
    #[test]
    fn swapping_the_sender_key_or_any_byte_breaks_it() {
        let (a, b, c) = keys();
        let body = seal(&env(&ALICE, &BOB, &TAG), &a, &b.public(), CONTENT_TEXT, b"hi").unwrap();
        let mut swapped = body.clone();
        swapped[2..34].copy_from_slice(&c.public());
        assert!(open(&env(&ALICE, &BOB, &TAG), &swapped, &[&b]).is_none());
        for i in [0, 1, 2, 40, 70, body.len() - 1] {
            let mut t = body.clone();
            t[i] ^= 1;
            assert!(open(&env(&ALICE, &BOB, &TAG), &t, &[&b]).is_none(), "byte {i}");
        }
        assert!(open(&env(&ALICE, &BOB, &TAG), &body[..body.len() - 1], &[&b]).is_none(), "truncated");
    }

    #[test]
    fn sizes_fall_in_buckets_and_the_limit_is_exact() {
        let (a, b, _) = keys();
        let e = env(&ALICE, &BOB, &TAG);
        let size = |n: usize| seal(&e, &a, &b.public(), CONTENT_TEXT, &vec![b'x'; n]).unwrap().len();
        assert_eq!(size(1), DM_HEADER + 64 + 16);
        assert_eq!(size(61), DM_HEADER + 64 + 16);
        assert_eq!(size(62), DM_HEADER + 128 + 16);
        assert_eq!(size(600), MAX_BODY);
        assert_eq!(size(MAX_CONTENT), MAX_BODY);
        assert_eq!(seal(&e, &a, &b.public(), CONTENT_TEXT, &vec![b'x'; MAX_CONTENT + 1]), Err(SealError::TooLong(MAX_CONTENT + 1)));
        assert_eq!(seal(&e, &a, &b.public(), CONTENT_TEXT, b""), Err(SealError::Empty));
        assert_eq!(MAX_CONTENT, 939);
    }

    #[test]
    fn padding_has_one_encoding() {
        assert_eq!(unpad(&[&[1, 0, 2][..], b"hi", &[0; 59]].concat()), Some((1, &b"hi"[..])));
        assert_eq!(unpad(&[&[1, 0, 2][..], b"hi", &[0; 58], &[1]].concat()), None, "non-zero padding");
        assert_eq!(unpad(&[&[1, 0, 2][..], b"hi", &[0; 60]].concat()), None, "not a bucket");
        assert_eq!(unpad(&[&[1, 0, 70][..], &[7; 70], &[0; 55]].concat()), Some((1, &[7u8; 70][..])), "70 bytes need the 128 bucket");
        assert_eq!(unpad(&[&[1, 0, 3][..], b"abc", &[0; 122]].concat()), None, "a short message padded to a larger bucket");
        assert_eq!(unpad(&[&[1, 0, 0][..], &[0; 61]].concat()), None, "empty");
    }

    #[test]
    fn announcements_verify_only_for_their_owner_chain_and_contract() {
        let id = IdentitySecret::from_seed(&[9; 32]);
        let (w, _, _) = keys();
        let a = Announcement::sign(&CTX, &ALICE, &id, 2900, 1, w.public());
        let body = a.encode();
        assert_eq!(body.len(), ANNOUNCEMENT_LEN);
        assert_eq!(Announcement::verify(&CTX, &ALICE, &body), Some(a.clone()));
        assert!(Announcement::verify(&CTX, &EVE, &body).is_none(), "re-posted by eve");
        assert!(Announcement::verify(&Context { chain_id: 15000, ..CTX }, &ALICE, &body).is_none(), "another chain");
        assert!(Announcement::verify(&Context { contract: [0; 20], ..CTX }, &ALICE, &body).is_none(), "another contract");
        for i in [0, 1, 2, 6, 10, 42, 74, 137] {
            let mut t = body.clone();
            t[i] ^= 1;
            assert!(Announcement::verify(&CTX, &ALICE, &t).is_none(), "byte {i}");
        }
        assert!(Announcement::verify(&CTX, &ALICE, &body[..137]).is_none());
        // Eve can announce Alice's identity key only by signing with it, which she cannot do.
        let eve_id = IdentitySecret::from_seed(&[8; 32]);
        let mut forged = Announcement::sign(&CTX, &EVE, &eve_id, 2900, 1, w.public());
        forged.identity = id.public();
        assert!(Announcement::verify(&CTX, &EVE, &forged.encode()).is_none());
    }

    #[test]
    fn fingerprints_bind_the_key_to_the_address() {
        let id = IdentitySecret::from_seed(&[9; 32]);
        let f = fingerprint(&id.public(), &ALICE);
        assert_eq!(f.split(' ').count(), 8);
        assert_eq!(f.len(), 39);
        assert_ne!(f, fingerprint(&id.public(), &EVE));
        assert_ne!(f, fingerprint(&IdentitySecret::from_seed(&[8; 32]).public(), &ALICE));
    }

    /// The published test vectors (`fixtures/messaging_v3_vectors.json`), which another
    /// implementation checks itself against: every announcement is reproduced byte for byte, every
    /// DM opens to its content (and the moved copies do not), every key and fingerprint matches.
    #[test]
    fn the_published_vectors_hold() {
        let v: serde_json::Value = serde_json::from_str(include_str!("../fixtures/messaging_v3_vectors.json")).unwrap();
        let b32 = |x: &serde_json::Value| -> [u8; 32] { hex::decode(x.as_str().unwrap()).unwrap().try_into().unwrap() };
        let b20 =
            |x: &serde_json::Value| -> [u8; 20] { hex::decode(x.as_str().unwrap().trim_start_matches("0x")).unwrap().try_into().unwrap() };
        let ctx = Context { chain_id: v["context"]["chain_id"].as_u64().unwrap(), contract: b20(&v["context"]["contract"]) };
        assert_eq!(hex::encode(keys_tag()), v["keys_tag"].as_str().unwrap());
        for k in v["weekly_keys"].as_array().unwrap() {
            let key = WeeklySecret::derive(&b32(&k["ikm"]));
            assert_eq!(
                (hex::encode(key.secret_bytes()), hex::encode(key.public())),
                (k["secret"].as_str().unwrap().into(), k["public"].as_str().unwrap().into())
            );
        }
        for a in v["announcements"].as_array().unwrap() {
            let id = IdentitySecret::from_seed(&b32(&a["identity_seed"]));
            let owner = b20(&a["owner"]);
            let signed = Announcement::sign(
                &ctx,
                &owner,
                &id,
                a["week"].as_u64().unwrap() as u32,
                a["sequence"].as_u64().unwrap() as u32,
                b32(&a["weekly"]),
            );
            assert_eq!(hex::encode(signed.encode()), a["body"].as_str().unwrap());
            assert_eq!(fingerprint(&id.public(), &owner), a["fingerprint"].as_str().unwrap());
            assert!(Announcement::verify(&ctx, &owner, &hex::decode(a["body"].as_str().unwrap()).unwrap()).is_some());
        }
        for d in v["messages"].as_array().unwrap() {
            let recipient_key = WeeklySecret::derive(&b32(&d["recipient_ikm"]));
            let (sender, recipient, tag) = (b20(&d["sender"]), b20(&d["recipient"]), b32(&d["tag"]));
            let body = hex::decode(d["body"].as_str().unwrap()).unwrap();
            let opened = open(&Envelope { ctx: &ctx, sender: &sender, recipient: &recipient, tag: &tag }, &body, &[&recipient_key]);
            match d["expect"].as_str() {
                Some("open") => {
                    let o = opened.expect("a vector that must open");
                    assert_eq!(o.content_type as u64, d["content_type"].as_u64().unwrap());
                    assert_eq!(String::from_utf8(o.content.to_vec()).unwrap(), d["content"].as_str().unwrap());
                    assert_eq!(hex::encode(o.sender_weekly), d["sender_weekly"].as_str().unwrap());
                }
                _ => assert!(opened.is_none(), "{} must not open", d["name"]),
            }
        }
    }

    /// Writes the vectors file. Run once, by hand, when the format changes:
    /// `QT_WRITE_VECTORS=1 cargo test -p wallet-core --lib write_the_vectors -- --ignored`
    #[test]
    #[ignore]
    fn write_the_vectors() {
        if std::env::var("QT_WRITE_VECTORS").is_err() {
            return;
        }
        let ctx = CTX;
        let (alice_ikm, bob_ikm) = ([0x11u8; 32], [0x22u8; 32]);
        let (a, b) = (WeeklySecret::derive(&alice_ikm), WeeklySecret::derive(&bob_ikm));
        let id = IdentitySecret::from_seed(&[0x33; 32]);
        let ann = Announcement::sign(&ctx, &ALICE, &id, 2959, 7, a.public());
        let tag = [0x44u8; 32];
        let good = seal(&env(&ALICE, &BOB, &tag), &a, &b.public(), CONTENT_TEXT, "gm, bob — meet at the usual place".as_bytes()).unwrap();
        let longest = "x".repeat(MAX_CONTENT);
        let long = seal(&env(&ALICE, &BOB, &tag), &a, &b.public(), CONTENT_TEXT, longest.as_bytes()).unwrap();
        let msg = |name: &str, sender: &[u8; 20], recipient: &[u8; 20], body: &[u8], expect: &str, content: &str| {
            serde_json::json!({"name": name, "sender": format!("0x{}", hex::encode(sender)), "recipient": format!("0x{}", hex::encode(recipient)),
                "tag": hex::encode(tag), "recipient_ikm": hex::encode(bob_ikm), "body": hex::encode(body), "expect": expect,
                "content_type": CONTENT_TEXT, "content": content, "sender_weekly": hex::encode(a.public())})
        };
        let weekly_keys: Vec<serde_json::Value> = [alice_ikm, bob_ikm]
            .iter()
            .map(|ikm| {
                let k = WeeklySecret::derive(ikm);
                serde_json::json!({"ikm": hex::encode(ikm), "secret": hex::encode(k.secret_bytes()), "public": hex::encode(k.public())})
            })
            .collect();
        let v = serde_json::json!({
            "format": "quai-messages v3 test vectors",
            "suite": "HPKE mode_auth, DHKEM(X25519, HKDF-SHA256), HKDF-SHA256, ChaCha20-Poly1305; Ed25519 announcements",
            "context": {"chain_id": ctx.chain_id, "contract": format!("0x{}", hex::encode(ctx.contract))},
            "keys_tag": hex::encode(keys_tag()),
            "weekly_keys": weekly_keys,
            "announcements": [{"owner": format!("0x{}", hex::encode(ALICE)), "identity_seed": hex::encode([0x33u8; 32]), "week": 2959, "sequence": 7,
                "weekly": hex::encode(a.public()), "body": hex::encode(ann.encode()), "fingerprint": fingerprint(&id.public(), &ALICE)}],
            "messages": [
                msg("alice to bob", &ALICE, &BOB, &good, "open", "gm, bob — meet at the usual place"),
                msg("the longest content", &ALICE, &BOB, &long, "open", &longest),
                msg("the same body posted by eve", &EVE, &BOB, &good, "reject", ""),
                msg("the same body claimed for eve", &ALICE, &EVE, &good, "reject", ""),
            ],
        });
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/fixtures/messaging_v3_vectors.json");
        std::fs::write(path, serde_json::to_string_pretty(&v).unwrap() + "\n").unwrap();
    }

    #[test]
    fn weeks_are_unix_weeks() {
        assert_eq!(week_of(0), 0);
        assert_eq!(week_of(WEEK_SECS - 1), 0);
        assert_eq!(week_of(WEEK_SECS), 1);
        assert_eq!(week_of(1_790_000_000), 2959);
    }
}
