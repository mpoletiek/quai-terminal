//! Where IPFS content is fetched from: two configurable gateways, `https://ipfs.qu.ai` by default.
//!
//! Two, because the wallet fetches two very different things and they do not want the same node:
//!
//! - **[`Content::Abi`]** — contract metadata, the standard-json a Quai contract's own bytecode
//!   commits to. `ipfs.qu.ai` is the authority for these: it is Quai's own gateway, the one
//!   `pushMetadataToIPFS` pins to and the one Quaiscan verifies against, so a contract deployed on
//!   this chain is there by construction. Pointing this elsewhere is for people running a node that
//!   pins Quai metadata themselves.
//! - **[`Content::Media`]** — everything else: NFT metadata, images, token logos. Bulky, from
//!   anywhere, and fetched constantly — the thing you actually want a local Kubo node for, and the
//!   one whose traffic says the most about you.
//!
//! NFT metadata and images often name `ipfs://<cid>`, which nothing on the web can fetch
//! directly; a gateway turns it into HTTP. The public one at ipfs.io is retiring (Sunset
//! 2026-09-21), and the answer its own notice gives is to run a node — so each gateway is a
//! setting: a local Kubo node (`http://127.0.0.1:8080`), one on the LAN, or any public gateway, in
//! either of the two URL styles gateways use:
//!
//! - **path** — `https://gateway.example/ipfs/<cid>/<path>`, what Kubo and most gateways serve;
//! - **subdomain** — `https://<cid>.ipfs.gateway.example/<path>`, written as a template with
//!   `{cid}` in place of the CID. A hostname label is case-insensitive, so a subdomain gateway
//!   needs a base32 CIDv1; the older base58 `Qm…` CIDs are converted.
//!
//! Plain `http://` is accepted only for this machine and private networks, where a node runs
//! without a certificate; anything public must be HTTPS. A gateway is untrusted: where a CID
//! commits to the raw bytes (a `raw` block hashed with SHA-256, the common `bafkrei…` form), what
//! comes back is checked against it, and content that does not match is refused.

use crate::error::{CoreError, Result};
use sha2::Digest;
use std::sync::RwLock;

/// The gateway used for contract metadata when none is configured: Quai's own, which is where
/// `pushMetadataToIPFS` pins and what Quaiscan verifies against.
pub const DEFAULT_ABI_GATEWAY: &str = "https://ipfs.qu.ai";

/// The gateway used for everything else when none is configured. Also `ipfs.qu.ai`: it serves any
/// CID, and the public gateway this used to point at (ipfs.io) sunsets 2026-09-21.
pub const DEFAULT_MEDIA_GATEWAY: &str = "https://ipfs.qu.ai";

/// Which of the two gateways a fetch goes through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Content {
    /// Contract metadata: the standard-json an address's bytecode names by CID.
    Abi,
    /// NFT metadata, images, token logos — anything that is not a contract's own metadata.
    Media,
}

impl Content {
    /// The built-in gateway for this kind of content.
    pub fn default_gateway(self) -> &'static str {
        match self {
            Content::Abi => DEFAULT_ABI_GATEWAY,
            Content::Media => DEFAULT_MEDIA_GATEWAY,
        }
    }

    /// The config key holding this gateway.
    pub fn config_key(self) -> &'static str {
        match self {
            Content::Abi => "abi_ipfs_gateway",
            Content::Media => "ipfs_gateway",
        }
    }

    /// How the setting is named to the user.
    pub fn label(self) -> &'static str {
        match self {
            Content::Abi => "contract ABIs",
            Content::Media => "images and NFT metadata",
        }
    }
}

/// A small document known to be on IPFS, used to test a gateway end to end: Quainance's metadata
/// for the CHEEZ launch. It is a raw SHA-256 CID, so the test proves the gateway returns the right
/// bytes, not just that it answers.
pub const TEST_CID: &str = "bafkreigynbdigag634tzagcofoclwl7yqaehe4gq4s4gnx6yxal764fzfa";

const RAW: u64 = 0x55;
const DAG_PB: u64 = 0x70;
const SHA2_256: u64 = 0x12;

/// A parsed content identifier: enough of one to convert it and to check bytes against it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cid {
    pub version: u8,
    pub codec: u64,
    pub hash_code: u64,
    pub digest: Vec<u8>,
}

impl Cid {
    /// Parse a CIDv0 (`Qm…`, base58btc) or a base32 CIDv1 (`b…`). Other multibase forms are rare
    /// in NFT metadata and are passed through untouched by path gateways without being parsed.
    pub fn parse(text: &str) -> Option<Cid> {
        if text.len() == 46 && text.starts_with("Qm") {
            let bytes = base58_decode(text)?;
            let (hash_code, digest) = multihash(&bytes)?;
            return Some(Cid { version: 0, codec: DAG_PB, hash_code, digest });
        }
        let body = text.strip_prefix('b')?;
        let bytes = base32_decode(&body.to_ascii_lowercase())?;
        let (version, rest) = varint(&bytes)?;
        let (codec, rest) = varint(rest)?;
        if version != 1 {
            return None;
        }
        let (hash_code, digest) = multihash(rest)?;
        Some(Cid { version: 1, codec, hash_code, digest })
    }

    /// A CID from its binary form, which is how one embedded in something else arrives — a
    /// contract's metadata tail carries the CID as bytes, not as text.
    ///
    /// A bare 34-byte SHA-256 multihash is the CIDv0 a `bytecodeHash: "ipfs"` build embeds; longer
    /// forms are read as a binary CIDv1 (version, codec, multihash).
    pub fn from_bytes(bytes: &[u8]) -> Option<Cid> {
        if bytes.len() == 34 && bytes[0] == SHA2_256 as u8 && bytes[1] == 32 {
            let (hash_code, digest) = multihash(bytes)?;
            return Some(Cid { version: 0, codec: DAG_PB, hash_code, digest });
        }
        let (version, rest) = varint(bytes)?;
        if version != 1 {
            return None;
        }
        let (codec, rest) = varint(rest)?;
        let (hash_code, digest) = multihash(rest)?;
        Some(Cid { version: 1, codec, hash_code, digest })
    }

    /// The CID as a gateway wants it: `Qm…` for a v0, `b…` base32 for a v1.
    pub fn to_text(&self) -> String {
        if self.version == 0 {
            let mut bytes = Vec::with_capacity(2 + self.digest.len());
            push_varint(&mut bytes, self.hash_code);
            push_varint(&mut bytes, self.digest.len() as u64);
            bytes.extend_from_slice(&self.digest);
            base58_encode(&bytes)
        } else {
            self.to_v1_base32()
        }
    }

    /// The same CID as base32 CIDv1 — the only form a hostname can carry.
    pub fn to_v1_base32(&self) -> String {
        let mut bytes = Vec::new();
        push_varint(&mut bytes, 1);
        push_varint(&mut bytes, self.codec);
        push_varint(&mut bytes, self.hash_code);
        push_varint(&mut bytes, self.digest.len() as u64);
        bytes.extend_from_slice(&self.digest);
        format!("b{}", base32_encode(&bytes))
    }

    /// Whether these bytes are exactly what the CID names. `None` when the CID does not commit to
    /// the bytes directly (a `dag-pb` file is a tree of blocks, not a hash of the file), so there
    /// is nothing to check without walking the DAG.
    pub fn verifies(&self, bytes: &[u8]) -> Option<bool> {
        (self.codec == RAW && self.hash_code == SHA2_256).then(|| sha2::Sha256::digest(bytes)[..] == self.digest[..])
    }

    /// Whether these bytes are the whole file a `dag-pb` CID names, for the one case that can be
    /// checked without the network: a file small enough to be a single UnixFS block.
    ///
    /// This is the case that matters for contract metadata — solc pins the metadata document with
    /// `bytecodeHash: "ipfs"`, and those documents are a few kilobytes — and it is what turns "the
    /// gateway handed this over" into "the bytecode committed to exactly these bytes". A file over
    /// [`UNIXFS_BLOCK`] is chunked into a tree whose root names other blocks, not the content, so
    /// there is nothing to check here and the answer is `None`.
    pub fn verifies_unixfs_file(&self, bytes: &[u8]) -> Option<bool> {
        if self.codec != DAG_PB || self.hash_code != SHA2_256 || bytes.len() > UNIXFS_BLOCK {
            return None;
        }
        Some(sha2::Sha256::digest(unixfs_file_block(bytes))[..] == self.digest[..])
    }

    /// Whichever check this CID supports: raw bytes, or a single-block UnixFS file. `None` when
    /// the CID commits to neither, so nothing was proven either way.
    pub fn verifies_content(&self, bytes: &[u8]) -> Option<bool> {
        self.verifies(bytes).or_else(|| self.verifies_unixfs_file(bytes))
    }
}

/// The largest file IPFS stores as one block under default chunking. Bigger files become a tree.
pub const UNIXFS_BLOCK: usize = 262_144;

/// The dag-pb block for a small file, for tests that need to compute a CID the way IPFS would.
#[cfg(test)]
pub(crate) fn unixfs_block_for_test(bytes: &[u8]) -> Vec<u8> {
    unixfs_file_block(bytes)
}

/// The dag-pb block IPFS makes of a small file: a `PBNode` (field 1, `Data`) wrapping a UnixFS
/// `Data` message that says "file", carries the bytes, and repeats their length.
fn unixfs_file_block(bytes: &[u8]) -> Vec<u8> {
    fn tag(out: &mut Vec<u8>, field: u8, wire: u8) {
        out.push((field << 3) | wire);
    }
    let mut unixfs = Vec::with_capacity(bytes.len() + 24);
    tag(&mut unixfs, 1, 0); // Type
    push_varint(&mut unixfs, 2); // File
    tag(&mut unixfs, 2, 2); // Data
    push_varint(&mut unixfs, bytes.len() as u64);
    unixfs.extend_from_slice(bytes);
    tag(&mut unixfs, 3, 0); // filesize
    push_varint(&mut unixfs, bytes.len() as u64);

    let mut node = Vec::with_capacity(unixfs.len() + 8);
    tag(&mut node, 1, 2); // PBNode.Data
    push_varint(&mut node, unixfs.len() as u64);
    node.extend_from_slice(&unixfs);
    node
}

/// A configured gateway.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gateway {
    scheme: String,
    /// The gateway's own host: for a subdomain gateway, the part after `{cid}.`.
    host: String,
    port: Option<u16>,
    /// A path prefix some gateways sit under (without `/ipfs`), no trailing slash.
    prefix: String,
    subdomain: bool,
}

impl Gateway {
    /// The built-in gateway for one kind of content.
    pub fn default_for(content: Content) -> Gateway {
        Gateway::parse(content.default_gateway()).unwrap_or_else(|_| Gateway {
            scheme: "https".into(),
            host: "ipfs.qu.ai".into(),
            port: None,
            prefix: String::new(),
            subdomain: false,
        })
    }

    /// Parse what a user typed: a base URL (`https://ipfs.io`, `http://127.0.0.1:8080`, with or
    /// without a trailing `/ipfs`) or a subdomain template (`https://{cid}.ipfs.dweb.link`).
    /// An empty string is refused here; the caller decides which default an empty setting means
    /// ([`Gateway::default_for`]), since the two kinds of content have different ones.
    pub fn parse(text: &str) -> Result<Gateway> {
        let text = text.trim();
        if text.is_empty() {
            return Err(CoreError::Invalid("IPFS gateway: empty".into()));
        }
        let bad = |why: &str| CoreError::Invalid(format!("IPFS gateway `{text}`: {why}"));
        let subdomain = text.contains("{cid}");
        let probe = text.replace("{cid}", "cidlabel");
        let url = reqwest::Url::parse(&probe).map_err(|_| bad("not a URL (e.g. http://127.0.0.1:8080 or https://ipfs.io)"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(bad("must be http:// or https://"));
        }
        if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
            return Err(bad("no credentials, query or fragment"));
        }
        let full_host = url.host_str().ok_or_else(|| bad("has no host"))?.to_lowercase();
        let host = if subdomain {
            full_host
                .strip_prefix("cidlabel.")
                .filter(|h| !h.is_empty())
                .ok_or_else(|| bad("`{cid}` must be the first part of the host"))?
                .to_string()
        } else {
            // Kubo redirects `localhost` to a `<cid>.ipfs.localhost` subdomain, which is another
            // host, and the wallet follows no redirect to another host. The address is the same.
            if full_host == "localhost" { "127.0.0.1".to_string() } else { full_host }
        };
        if url.path().contains("cidlabel") {
            return Err(bad("`{cid}` goes in the host; path gateways need only their base URL"));
        }
        if url.scheme() == "http" && !is_local(&host) {
            return Err(bad("a public gateway must use https:// (http is for a node on this machine or your network)"));
        }
        let prefix = url.path().trim_end_matches('/').trim_end_matches("/ipfs").trim_end_matches('/').to_string();
        Ok(Gateway { scheme: url.scheme().into(), host, port: url.port(), prefix, subdomain })
    }

    /// The URL for `cid` and an optional path inside it (`/1.png`, or empty).
    pub fn url(&self, cid: &str, rest: &str) -> Result<String> {
        let port = self.port.map(|p| format!(":{p}")).unwrap_or_default();
        if self.subdomain {
            let label = Cid::parse(cid)
                .map(|c| c.to_v1_base32())
                .ok_or_else(|| CoreError::Invalid(format!("CID {cid} cannot be used with a subdomain gateway")))?;
            let path = if rest.is_empty() { "/".to_string() } else { rest.to_string() };
            Ok(format!("{}://{label}.{}{port}{}{path}", self.scheme, self.host, self.prefix))
        } else {
            Ok(format!("{}://{}{port}{}/ipfs/{cid}{rest}", self.scheme, self.host, self.prefix))
        }
    }

    /// The gateway's host (for a subdomain gateway, the host every CID sits under).
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Whether a URL is one this gateway serves: the exact host, or a CID subdomain of it.
    pub fn serves(&self, url: &str) -> bool {
        let Ok(parsed) = reqwest::Url::parse(url) else { return false };
        let Some(host) = parsed.host_str().map(str::to_lowercase) else { return false };
        parsed.scheme() == self.scheme
            && parsed.port() == self.port
            && (host == self.host || (self.subdomain && host.ends_with(&format!(".{}", self.host))))
    }

    /// On this machine or a private network: not a third party, so it is not rate-limited like
    /// one and is reached directly rather than through a privacy proxy — the same as node RPC.
    pub fn is_local(&self) -> bool {
        is_local(&self.host)
    }

    /// As the user would type it back.
    pub fn display(&self) -> String {
        let port = self.port.map(|p| format!(":{p}")).unwrap_or_default();
        if self.subdomain {
            format!("{}://{{cid}}.{}{port}{}", self.scheme, self.host, self.prefix)
        } else {
            format!("{}://{}{port}{}", self.scheme, self.host, self.prefix)
        }
    }

    /// Whether this is the built-in default for that kind of content.
    pub fn is_default_for(&self, content: Content) -> bool {
        *self == Gateway::default_for(content)
    }
}

/// This machine or a private network: loopback, RFC 1918, link-local, IPv6 unique-local, and
/// `localhost` names. Only IP literals and localhost count — a public name that happens to
/// resolve privately is still treated as public, since the wallet does not resolve names itself.
pub fn is_local(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00 || (ip.segments()[0] & 0xffc0) == 0xfe80,
        Err(_) => false,
    }
}

static ABI_GATEWAY: RwLock<Option<Gateway>> = RwLock::new(None);
static MEDIA_GATEWAY: RwLock<Option<Gateway>> = RwLock::new(None);

fn slot(content: Content) -> &'static RwLock<Option<Gateway>> {
    match content {
        Content::Abi => &ABI_GATEWAY,
        Content::Media => &MEDIA_GATEWAY,
    }
}

/// Tests that read or change the process-wide gateway take this, so they do not race.
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Use this gateway for every fetch of `content` in the process from now on (None or empty: the
/// built-in default for that content). Unlike the proxy, it can change while running: a picture
/// fetched from the old one is still the same content, so nothing already cached is wrong.
pub fn set_gateway(content: Content, text: Option<&str>) -> Result<()> {
    let parsed = match text.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => Some(Gateway::parse(t)?),
        None => None,
    };
    if let Ok(mut g) = slot(content).write() {
        *g = parsed;
    }
    Ok(())
}

/// The gateway in use for this kind of content.
pub fn gateway(content: Content) -> Gateway {
    slot(content).read().ok().and_then(|g| g.clone()).unwrap_or_else(|| Gateway::default_for(content))
}

/// A request to IPFS resolved against the gateway: where to fetch it, and — when the CID commits to
/// the bytes — what to check them against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Located {
    pub url: String,
    pub verify: Option<Cid>,
}

/// Resolve `<cid>[/path]` (the part after `ipfs://`) against the gateway for that content.
pub fn locate(content: Content, cid_path: &str) -> Result<Located> {
    let cid_path = cid_path.trim_start_matches("ipfs/");
    if cid_path.is_empty() || !cid_path.chars().all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c)) || cid_path.contains("..") {
        return Err(CoreError::Invalid("malformed ipfs URI".into()));
    }
    let (cid, rest) = match cid_path.find('/') {
        Some(i) => (&cid_path[..i], &cid_path[i..]),
        None => (cid_path, ""),
    };
    let url = gateway(content).url(cid, rest)?;
    // Only a whole object can be checked against its CID; a path inside one names another block.
    //
    // `verifies_content`, not `verifies`: the latter answers only for a `raw` CID, which would
    // drop every `dag-pb` CID — including every solc metadata CID, the one case this exists for.
    let verify = rest.is_empty().then(|| Cid::parse(cid)).flatten().filter(|c| c.verifies_content(&[]).is_some());
    Ok(Located { url, verify })
}

/// A public gateway URL (`https://ipfs.io/ipfs/<cid>/…`) as the `<cid>/…` it names, so metadata
/// that hard-codes the public gateway still goes to the one configured.
pub fn from_public_url(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://ipfs.io/ipfs/")?;
    (!rest.is_empty()).then_some(rest)
}

/// Whether a media reference will be fetched from an IPFS gateway: `ipfs://`, a public gateway
/// link, or a link to either configured gateway. Those are paced separately from everything else.
pub fn is_gateway_url(url: &str) -> bool {
    url.starts_with("ipfs://") || from_public_url(url).is_some() || gateway(Content::Media).serves(url) || gateway(Content::Abi).serves(url)
}

/// The host whose request budget a request counts against. A subdomain gateway puts every CID
/// on its own hostname, and budgets are kept per host: without this, each picture would arrive
/// with a fresh budget and a subdomain gateway would never be paced at all.
pub fn budget_host(host: &str) -> String {
    for content in [Content::Media, Content::Abi] {
        let gateway = gateway(content);
        if gateway.subdomain && host.ends_with(&format!(".{}", gateway.host)) {
            return gateway.host;
        }
    }
    host.to_string()
}

/// How a gateway test went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestOutcome {
    /// It served the test document, byte for byte (milliseconds taken).
    Verified(u128),
    /// It answered, but the test document did not come back (not found, timed out): the gateway
    /// works, it just could not find this one piece of content in time.
    Answered(String),
}

/// Fetch [`TEST_CID`] through `gateway` and check it against its CID. `Err` when the gateway could
/// not be reached at all, or returned something other than what the CID names — the two cases in
/// which it should not be used.
pub async fn test(gateway: &Gateway) -> Result<TestOutcome> {
    test_with(gateway, TEST_CID).await
}

/// [`test`] with any raw SHA-256 CID.
pub async fn test_with(gateway: &Gateway, test_cid: &str) -> Result<TestOutcome> {
    let url = gateway.url(test_cid, "")?;
    let started = std::time::Instant::now();
    match crate::http::get(&url, 64 * 1024).await {
        Ok(fetched) => {
            let cid = Cid::parse(test_cid).ok_or_else(|| CoreError::Invalid("test CID".into()))?;
            if cid.verifies(&fetched.bytes) == Some(true) {
                Ok(TestOutcome::Verified(started.elapsed().as_millis()))
            } else {
                Err(CoreError::Rejected(format!("{} returned content that does not match the CID it was asked for", gateway.display())))
            }
        }
        Err(e) if crate::http::unreachable(&e) => Err(e),
        Err(e) => Ok(TestOutcome::Answered(e.to_string())),
    }
}

/// One line per gateway for `data test` and System › Data sources, each tested. Both are listed
/// even when they are the same host: they are separate settings and either can be changed alone.
pub async fn test_lines() -> Vec<(String, Result<String>)> {
    let mut out = Vec::new();
    for content in [Content::Abi, Content::Media] {
        let gateway = gateway(content);
        let label = format!("IPFS {} ({})", gateway.display(), content.label());
        let result = test(&gateway).await.map(|outcome| match outcome {
            TestOutcome::Verified(ms) => format!("test file verified against its CID in {ms} ms"),
            TestOutcome::Answered(why) => format!("reachable, but the test file did not arrive ({why})"),
        });
        out.push((label, result));
    }
    out
}

// ---------------------------------------------------------------- encodings

fn varint(bytes: &[u8]) -> Option<(u64, &[u8])> {
    let mut value = 0u64;
    for (i, b) in bytes.iter().enumerate().take(9) {
        value |= u64::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            return Some((value, &bytes[i + 1..]));
        }
    }
    None
}

fn push_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn multihash(bytes: &[u8]) -> Option<(u64, Vec<u8>)> {
    let (code, rest) = varint(bytes)?;
    let (len, rest) = varint(rest)?;
    (rest.len() == len as usize && len > 0).then(|| (code, rest.to_vec()))
}

const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

fn base32_encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    let (mut buffer, mut bits) = (0u32, 0u32);
    for b in bytes {
        buffer = (buffer << 8) | u32::from(*b);
        bits += 8;
        while bits >= 5 {
            out.push(BASE32[((buffer >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(BASE32[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn base32_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut buffer, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let v = BASE32.iter().position(|x| *x == c)? as u32;
        buffer = (buffer << 5) | v;
        bits += 5;
        if bits >= 8 {
            out.push((buffer >> (bits - 8)) as u8);
            bits -= 8;
        }
        buffer &= (1 << bits) - 1;
    }
    Some(out)
}

const BASE58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

fn base58_encode(bytes: &[u8]) -> String {
    let mut digits: Vec<u8> = Vec::new();
    for b in bytes {
        let mut carry = u32::from(*b);
        for d in digits.iter_mut() {
            carry += u32::from(*d) << 8;
            *d = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let mut out = String::with_capacity(digits.len() + 1);
    // Every leading zero byte is one leading `1`, which the arithmetic above drops.
    out.extend(bytes.iter().take_while(|b| **b == 0).map(|_| '1'));
    out.extend(digits.iter().rev().map(|d| BASE58[*d as usize] as char));
    out
}

fn base58_decode(text: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = BASE58;
    let mut bytes: Vec<u8> = Vec::new();
    for c in text.bytes() {
        let mut carry = ALPHABET.iter().position(|x| *x == c)? as u32;
        for b in bytes.iter_mut().rev() {
            carry += u32::from(*b) * 58;
            *b = carry as u8;
            carry >>= 8;
        }
        while carry > 0 {
            bytes.insert(0, carry as u8);
            carry >>= 8;
        }
    }
    let zeros = text.bytes().take_while(|c| *c == b'1').count();
    let mut out = vec![0u8; zeros];
    out.extend(bytes);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The well-known conversion every subdomain gateway performs, checked both ways.
    #[test]
    fn a_v0_cid_becomes_the_v1_a_hostname_can_carry() {
        let v0 = Cid::parse("QmdfTbBqBPQ7VNxZEYEj14VmRuZBkqFbiwReogJgS1zR1n").unwrap();
        assert_eq!((v0.version, v0.codec, v0.hash_code, v0.digest.len()), (0, DAG_PB, SHA2_256, 32));
        assert_eq!(v0.to_v1_base32(), "bafybeihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku");
        let v1 = Cid::parse("bafybeihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku").unwrap();
        assert_eq!((v1.digest.clone(), v1.codec), (v0.digest, DAG_PB));
        assert_eq!(Cid::parse("not a cid"), None);
        assert_eq!(Cid::parse("bafy!!"), None);
    }

    /// The CID a contract's metadata tail carries, read from bytes and turned back into the text a
    /// gateway wants. Taken from the message board on mainnet (`0x0077AD43…`, solc 0.8.20).
    #[test]
    fn a_cid_embedded_as_bytes_round_trips_to_its_text() {
        let embedded = hex::decode("1220a2b1a1fc77c6cbda58759187ebbe6772f0dfda6523395e39f41da12baea66ecc").unwrap();
        let cid = Cid::from_bytes(&embedded).expect("a v0 multihash");
        assert_eq!((cid.version, cid.codec, cid.hash_code), (0, DAG_PB, SHA2_256));
        assert_eq!(cid.to_text(), "QmZHjrbTYGTTNfL9SoX3E3MQf2PdpB7iVwrax8qBdzj7DV");
        assert_eq!(Cid::parse(&cid.to_text()), Some(cid), "text and bytes name the same CID");
        // A binary CIDv1 (version, codec, multihash) is read too.
        let mut v1 = vec![0x01, 0x55];
        v1.extend_from_slice(&hex::decode("1220a2b1a1fc77c6cbda58759187ebbe6772f0dfda6523395e39f41da12baea66ecc").unwrap());
        let cid = Cid::from_bytes(&v1).expect("a v1 CID");
        assert_eq!((cid.version, cid.codec), (1, RAW));
        assert!(cid.to_text().starts_with('b'));
        assert_eq!(Cid::from_bytes(&[]), None);
    }

    /// A small file's dag-pb CID is computable, so metadata a gateway hands over can be checked
    /// against what the bytecode committed to rather than taken on trust.
    #[test]
    fn a_single_block_file_is_checked_against_its_dag_pb_cid() {
        // The CIDv0 `ipfs add` gives for "hello world\n" — the canonical worked example.
        let cid = Cid::parse("QmT78zSuBmuS4z925WZfrqQ1qHaJ56DQaTfyMUF7F8ff5o").unwrap();
        assert_eq!(cid.verifies_unixfs_file(b"hello world\n"), Some(true));
        assert_eq!(cid.verifies_unixfs_file(b"hello world"), Some(false));
        assert_eq!(cid.verifies(b"hello world\n"), None, "it is not a raw CID");
        assert_eq!(cid.verifies_content(b"hello world\n"), Some(true), "and the combined check finds the right one");
        // Too big to be one block: a tree, whose root names blocks rather than the bytes.
        assert_eq!(cid.verifies_unixfs_file(&vec![0u8; UNIXFS_BLOCK + 1]), None);
        // A raw CID still goes through the raw check.
        let raw = Cid::parse("bafkreifzjut3te2nhyekklss27nh3k72ysco7y32koao5eei66wof36n5e").unwrap();
        assert_eq!(raw.verifies_content(b"hello world"), Some(true));
    }

    /// A raw SHA-256 CID pins the bytes: the right ones pass, anything else does not. A dag-pb CID
    /// names a tree of blocks and cannot be checked from the file alone, so it says nothing.
    #[test]
    fn raw_content_is_checked_against_its_cid() {
        // CIDv1 raw sha2-256 of "hello world" (no newline).
        let cid = Cid::parse("bafkreifzjut3te2nhyekklss27nh3k72ysco7y32koao5eei66wof36n5e").unwrap();
        assert_eq!(cid.verifies(b"hello world"), Some(true));
        assert_eq!(cid.verifies(b"hello world!"), Some(false));
        assert_eq!(Cid::parse("QmdfTbBqBPQ7VNxZEYEj14VmRuZBkqFbiwReogJgS1zR1n").unwrap().verifies(b"x"), None);
    }

    #[test]
    fn gateways_parse_in_both_styles() {
        let default = Gateway::default_for(Content::Media);
        assert_eq!(default.url("QmAbc", "/1.png").unwrap(), "https://ipfs.qu.ai/ipfs/QmAbc/1.png");
        assert!(default.is_default_for(Content::Media) && !default.is_local());
        // The two kinds of content have their own setting, and both start at Quai's gateway.
        assert!(Gateway::default_for(Content::Abi).is_default_for(Content::Abi));
        // A local Kubo node: http is fine here, `localhost` is pinned to 127.0.0.1, `/ipfs` is optional.
        let kubo = Gateway::parse("http://localhost:8080/ipfs/").unwrap();
        assert_eq!(kubo.url("bafkrei1", "").unwrap(), "http://127.0.0.1:8080/ipfs/bafkrei1");
        assert!(kubo.is_local() && kubo.serves("http://127.0.0.1:8080/ipfs/x") && !kubo.serves("https://127.0.0.1:8080/ipfs/x"));
        assert_eq!(Gateway::parse("http://10.0.0.12:8080").unwrap().display(), "http://10.0.0.12:8080");
        // A gateway under a path prefix keeps it.
        assert_eq!(Gateway::parse("https://gw.example/pub").unwrap().url("QmA", "").unwrap(), "https://gw.example/pub/ipfs/QmA");
        // Subdomain gateways take a template and get a base32 v1 label, even for a v0 CID.
        let sub = Gateway::parse("https://{cid}.ipfs.dweb.link").unwrap();
        assert_eq!(
            sub.url("QmdfTbBqBPQ7VNxZEYEj14VmRuZBkqFbiwReogJgS1zR1n", "/a.png").unwrap(),
            "https://bafybeihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku.ipfs.dweb.link/a.png"
        );
        assert!(sub.serves("https://bafyabc.ipfs.dweb.link/") && !sub.serves("https://evil.link/"));
        assert_eq!(sub.display(), "https://{cid}.ipfs.dweb.link");
        assert!(sub.url("zNotBase32", "").is_err(), "a CID a hostname cannot carry is refused, not mangled");
        // Refusals: public http, other schemes, credentials, a misplaced {cid}.
        for bad in [
            "http://ipfs.io",
            "ftp://127.0.0.1",
            "https://user:pw@gw.example",
            "https://gw.example/?x=1",
            "https://gw.example/ipfs/{cid}",
            "https://ipfs.{cid}.example",
            "nonsense",
        ] {
            assert!(Gateway::parse(bad).is_err(), "{bad}");
        }
        assert!(Gateway::parse("  ").is_err(), "empty is the caller's default to choose, not one this can guess");
    }

    /// A stand-in gateway on loopback answering every request with `status` and `body`.
    async fn fake_gateway(status: u16, body: &'static [u8]) -> Gateway {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                let _ = socket.read(&mut buf).await;
                let head = format!("HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(body).await;
            }
        });
        Gateway::parse(&format!("http://127.0.0.1:{port}")).unwrap()
    }

    /// The four answers a gateway test can give, and which of them keep a gateway out.
    #[tokio::test]
    async fn a_gateway_is_tested_on_what_it_returns() {
        let hello = "bafkreifzjut3te2nhyekklss27nh3k72ysco7y32koao5eei66wof36n5e";
        let honest = fake_gateway(200, b"hello world").await;
        assert!(matches!(test_with(&honest, hello).await, Ok(TestOutcome::Verified(_))));
        // Reachable, but it does not have the file (a fresh node, say): usable, with a warning.
        let empty = fake_gateway(404, b"").await;
        assert!(matches!(test_with(&empty, hello).await, Ok(TestOutcome::Answered(_))));
        // Wrong bytes: never used.
        let liar = fake_gateway(200, b"hello world!").await;
        assert!(matches!(test_with(&liar, hello).await, Err(CoreError::Rejected(_))));
        // Nothing listening: never used, and said plainly.
        let nothing = Gateway::parse("http://127.0.0.1:1").unwrap();
        let refused = test_with(&nothing, hello).await;
        assert!(refused.as_ref().is_err_and(crate::http::unreachable), "{refused:?}");
    }

    /// Every CID on a subdomain gateway shares the gateway's one request budget.
    #[test]
    fn a_subdomain_gateway_is_paced_as_one_host() {
        let _gateway = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_gateway(Content::Media, Some("https://{cid}.ipfs.dweb.link")).unwrap();
        assert_eq!(budget_host("bafyone.ipfs.dweb.link"), "ipfs.dweb.link");
        assert_eq!(budget_host("bafytwo.ipfs.dweb.link"), "ipfs.dweb.link");
        assert_eq!(budget_host("explorer.qu.ai"), "explorer.qu.ai", "nothing else is folded");
        set_gateway(Content::Media, None).unwrap();
        assert_eq!(budget_host("bafyone.ipfs.dweb.link"), "bafyone.ipfs.dweb.link");
        // The ABI gateway is paced the same way, on its own setting.
        set_gateway(Content::Abi, Some("https://{cid}.ipfs.example")).unwrap();
        assert_eq!(budget_host("bafyone.ipfs.example"), "ipfs.example");
        set_gateway(Content::Abi, None).unwrap();
    }

    #[test]
    fn local_means_this_machine_or_a_private_network() {
        for local in
            ["127.0.0.1", "10.0.0.12", "192.168.1.5", "172.16.0.1", "169.254.1.1", "::1", "[::1]", "fd00::1", "localhost", "ipfs.localhost"]
        {
            assert!(is_local(local), "{local}");
        }
        for public in ["ipfs.io", "8.8.8.8", "172.32.0.1", "2001:db8::1", "localhost.example.com"] {
            assert!(!is_local(public), "{public}");
        }
    }

    #[test]
    fn ipfs_references_locate_against_the_gateway_and_whole_raw_objects_are_checked() {
        let located = locate(Content::Media, "bafkreifzjut3te2nhyekklss27nh3k72ysco7y32koao5eei66wof36n5e").unwrap();
        assert!(located.url.ends_with("/ipfs/bafkreifzjut3te2nhyekklss27nh3k72ysco7y32koao5eei66wof36n5e"));
        assert!(located.verify.is_some(), "a whole raw object is checked");
        assert!(locate(Content::Media, "QmAbc/1.png").unwrap().verify.is_none(), "a path inside a DAG cannot be");
        // A dag-pb CIDv0 — every solc metadata CID — must also come back with a verifier, or the
        // check the contract feature is built on never runs.
        let metadata = locate(Content::Abi, "QmZHjrbTYGTTNfL9SoX3E3MQf2PdpB7iVwrax8qBdzj7DV").unwrap();
        let cid = metadata.verify.expect("a whole dag-pb object is checked too");
        assert_eq!(cid.codec, DAG_PB);
        for bad in ["", "Qm/../../etc", "Qm?x=1", "Qm abc"] {
            assert!(locate(Content::Media, bad).is_err(), "{bad:?}");
        }
        assert_eq!(from_public_url("https://ipfs.io/ipfs/QmAbc/1.png"), Some("QmAbc/1.png"));
        assert_eq!(from_public_url("https://explorer.qu.ai/x"), None);
    }
}
