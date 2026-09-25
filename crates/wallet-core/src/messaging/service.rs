//! Messaging v3 as the wallet uses it: publishing weekly keys, sending, reading the chain for what
//! arrived, and the local decisions (accept, block, trust, verify) that no transaction ever records.
//!
//! Messages go from the account the user chose (`@`, `account use`), and every account is an
//! identity of its own: its own keys, conversations and requests. Choosing another account
//! switches all of that; nothing is moved or deleted.
//!
//! Anything that touches a key needs the wallet unlocked: an account's key file is sealed under
//! that account's own key. Listing conversations reads only the local store, but that store is
//! encrypted under a key inside the key file, so it needs the unlock too.

use super::keys::{self, KeyFile};
use super::store::{KnownKey, MessageRecord, PeerRecord, PeerState, Store};
use super::wire;
use crate::appdb::OpStatus;
use crate::error::{CoreError, Result};
use crate::registry::now;
use crate::session::Session;
use crate::tx::{Review, field};
use quai_sdk::provider::{Log, LogFilter, LogRange, TopicMatch};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use zeroize::Zeroizing;

/// The widest block range the public RPC serves in one `quai_getLogs` (it refuses more).
pub const LOG_PAGE: u64 = 10_000;
/// Blocks are about five seconds apart.
const BLOCK_SECS: u64 = 5;
/// How far back a peer's key is searched for before saying they have none.
pub const KEY_LOOKBACK: u64 = 4 * wire::WEEK_SECS / BLOCK_SECS;
/// A peer whose newest key is older than this is probably away: the review says so.
pub const STALE_KEY_SECS: u64 = 14 * 24 * 3600;
/// How far the reader steps back when the block it stopped at is no longer canonical.
const REORG_REWIND: u64 = 64;
/// An outgoing message still unsent after this was never sent (its review was left open).
const UNSENT_AFTER: u64 = 3600;
/// The review's fields that are shown once and never journaled.
const PRIVATE_FIELDS: [&str; 3] = ["To", "Fingerprint", "Message"];

/// What messaging needs before a message can go.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KeyNeed {
    /// This computer holds no messaging keys for the account: it has not messaged from here yet
    /// (or they went with a restore). Publishing this week's key makes them.
    NoKeys,
    /// This week's key is not on chain yet: publish it first.
    Publish,
    /// The announcement is sent and not yet mined. A message can follow: the account's nonce
    /// orders it after the announcement.
    Publishing,
    Ready,
}

/// Where messaging stands, for `message status` and the TUI.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub account: Option<String>,
    pub need: KeyNeed,
    /// This wallet's fingerprint, when unlocked and set up.
    pub fingerprint: Option<String>,
    /// The newest weekly key: (sequence, week, on chain).
    pub key: Option<(u32, u32, bool)>,
    pub keys_held: usize,
    /// Incoming messages have been read up to this block.
    pub scanned_to: Option<u64>,
}

/// What a sync found.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SyncReport {
    /// New messages per accepted peer.
    pub arrived: Vec<(String, usize)>,
    /// New messages from people not yet accepted.
    pub requests: usize,
    /// Messages from blocked addresses, dropped unread.
    pub blocked: usize,
    /// Bodies that opened but whose sender key the sending address never announced.
    pub rejected: usize,
    /// Weekly keys deleted because their grace ran out.
    pub keys_deleted: usize,
    pub scanned_to: u64,
}

/// One conversation in the list.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conversation {
    pub peer: String,
    /// The contact's name, when the address is in the address book.
    pub name: Option<String>,
    pub state: PeerState,
    pub fingerprint: Option<String>,
    pub verified: bool,
    pub identity_changed: bool,
    pub messages: usize,
    pub unread: usize,
    pub last_at: u64,
}

/// One message as shown.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Line {
    pub at: u64,
    pub outgoing: bool,
    pub text: String,
    /// `received`, or for an outgoing message its operation's status (`pending`, `sent`,
    /// `failed`, `not sent`).
    pub status: String,
    /// Opened under an identity key the user has not accepted.
    pub unverified: bool,
    pub tx: String,
}

/// A fingerprint comparison.
#[derive(Clone, Debug, Serialize)]
pub struct Fingerprints {
    pub peer: String,
    pub theirs: Option<String>,
    pub ours: String,
    pub verified: bool,
}

/// The key file, open, with what saving it back needs.
struct Opened {
    file: KeyFile,
    wrap: Zeroizing<[u8; 32]>,
    binding: String,
    path: std::path::PathBuf,
}

impl Opened {
    fn save(&self) -> Result<()> {
        self.file.save(&self.path, &self.wrap, &self.binding)
    }
}

/// Where messaging kept its one account before every account messaged for itself.
fn kv_legacy_account(network: &str) -> String {
    format!("messaging_account:{network}")
}

/// The files of one account's messaging identity.
const IDENTITY_FILES: [&str; 5] = ["keys.sealed", "messaging.sqlite", "messaging.sqlite-wal", "messaging.sqlite-shm", ".lock"];

fn topic_of_address(address: &str) -> String {
    format!("0x{:0>64}", address.trim_start_matches("0x").to_lowercase())
}

fn topic_of_kind(kind: u8) -> String {
    format!("0x{kind:064x}")
}

/// Message text as shown: valid UTF-8, line breaks kept as spaces, control characters dropped.
fn display_text(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let text: String = text.chars().map(|c| if matches!(c, '\n' | '\r' | '\t') { ' ' } else { c }).collect();
    let clean = crate::explorer::clean_text(&text);
    (!clean.trim().is_empty()).then_some(clean)
}

impl Session {
    /// This network's messaging folder, holding a folder per account.
    fn messaging_root(&self) -> std::path::PathBuf {
        self.registry.paths().wallet_dir(&self.meta.id).join("messaging").join(&self.network.id)
    }

    /// The folder of the account messages go from.
    fn messaging_dir(&self) -> Result<std::path::PathBuf> {
        let account = self.require_messaging_account()?;
        let dir = self.messaging_root().join(&account);
        self.adopt_legacy_identity(&account, &dir);
        Ok(dir)
    }

    /// Messaging once went from one account chosen for it, with its files straight under the
    /// network's folder. They move into that account's own folder, so its identity carries on.
    fn adopt_legacy_identity(&self, account: &str, dir: &std::path::Path) {
        let key = kv_legacy_account(&self.network.id);
        let Some(legacy) = self.app.kv(&key).ok().flatten().map(|a| a.to_lowercase()) else { return };
        let root = self.messaging_root();
        if legacy.is_empty() || !root.join("keys.sealed").exists() {
            let _ = self.app.set_kv(&key, "");
            return;
        }
        let target = if legacy == account { dir.to_path_buf() } else { root.join(&legacy) };
        if target.join("keys.sealed").exists() || crate::paths::ensure_private_dir(&target).is_err() {
            return;
        }
        for file in IDENTITY_FILES {
            let _ = std::fs::rename(root.join(file), target.join(file));
        }
        let _ = self.app.set_kv(&key, "");
    }

    /// The account messages and board posts go from: the one the user chose, lowercase.
    pub fn messaging_account(&self) -> Option<String> {
        self.meta.default_quai_account().ok().map(|a| a.address.to_lowercase())
    }

    /// [`Self::messaging_account`], or an error when the wallet has no Quai account to post from.
    pub fn require_messaging_account(&self) -> Result<String> {
        self.messaging_account().ok_or_else(|| CoreError::NotFound("this wallet has no Quai account to message from".into()))
    }

    /// Whether the account messages go from has keys on this computer. Needs no unlock.
    pub fn has_messaging_keys(&self) -> bool {
        self.messaging_dir().is_ok_and(|d| d.join("keys.sealed").exists())
    }

    fn account_secret(&self, address: &str) -> Result<Zeroizing<[u8; 32]>> {
        let account = self.meta.find_quai_account(address)?;
        let parsed = account.address.parse().map_err(|_| CoreError::Invalid("account address".into()))?;
        let key = self.keys()?.quai_key(parsed, account.hd_index)?;
        Ok(Zeroizing::new(*key.export_bytes().as_bytes()))
    }

    fn open_key_file(&self) -> Result<Option<Opened>> {
        let account = self.require_messaging_account()?;
        let binding = keys::binding(&self.meta.id, &self.network.id, &account);
        let wrap = keys::wrap_key(&*self.account_secret(&account)?, &binding);
        let path = self.messaging_dir()?.join("keys.sealed");
        Ok(KeyFile::load(&path, &wrap, &binding)?.map(|file| Opened { file, wrap, binding, path }))
    }

    fn require_key_file(&self) -> Result<Opened> {
        let account = self.require_messaging_account()?;
        self.open_key_file()?.ok_or_else(|| {
            CoreError::NotFound(format!(
                "{} has not messaged from this computer yet: `quai-terminal message keys` publishes its key",
                crate::session::short_address(&account)
            ))
        })
    }

    /// Make the account's messaging identity if this computer has none for it; true when it was
    /// made now. Nothing is on chain until its first key is published.
    async fn ensure_key_file(&self) -> Result<bool> {
        if self.open_key_file()?.is_some() {
            return Ok(false);
        }
        // The board must be configured and its code must match the pin before anything is made.
        self.messaging_context().await?;
        let head = self.head_header().await?;
        let account = self.require_messaging_account()?;
        let dir = self.messaging_dir()?;
        crate::paths::ensure_private_dir(&dir)?;
        let lock = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join(".lock"))?;
        lock.lock()?;
        if self.open_key_file()?.is_some() {
            return Ok(false);
        }
        // Whatever an earlier identity left here goes with it.
        for file in ["messaging.sqlite", "messaging.sqlite-wal", "messaging.sqlite-shm"] {
            let _ = std::fs::remove_file(dir.join(file));
        }
        let binding = keys::binding(&self.meta.id, &self.network.id, &account);
        let wrap = keys::wrap_key(&*self.account_secret(&account)?, &binding);
        let mut file = KeyFile::new(&account, now())?;
        file.ensure_week(wire::week_of(now()))?;
        file.save(&dir.join("keys.sealed"), &wrap, &binding)?;
        // Nothing can have been sealed to keys that did not exist: reading starts here.
        let store = self.open_store(&file)?;
        store.set_scanned(head.number, &head.hash.to_string().to_lowercase())?;
        store.set_origin(head.number)?;
        Ok(true)
    }

    /// Change the key file under an exclusive lock, so two processes never make two keys for one
    /// week or undo each other's deletions.
    fn with_key_file<R>(&self, f: impl FnOnce(&mut KeyFile) -> Result<R>) -> Result<R> {
        let dir = self.messaging_dir()?;
        crate::paths::ensure_private_dir(&dir)?;
        let lock = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join(".lock"))?;
        lock.lock()?;
        let mut opened = self.require_key_file()?;
        let r = f(&mut opened.file)?;
        opened.save()?;
        Ok(r)
    }

    fn open_store(&self, file: &KeyFile) -> Result<Store> {
        Store::open(&self.messaging_dir()?.join("messaging.sqlite"), file.store_key())
    }

    async fn messaging_context(&self) -> Result<(wire::Context, quai_sdk::QuaiAddress)> {
        let pin = self
            .network
            .ecosystem
            .messages
            .clone()
            .ok_or_else(|| CoreError::NotFound(format!("no message board is configured on {}", self.network.id)))?;
        let address =
            crate::data::verify_pinned(&self.app, &self.node, &self.network, &pin, "message board", crate::data::Trust::FirstHand).await?;
        let contract = wire::address_bytes(&address.to_string()).ok_or_else(|| CoreError::Invalid("message board address".into()))?;
        Ok((wire::Context { chain_id: self.network.chain_id, contract }, address))
    }

    async fn head_header(&self) -> Result<quai_sdk::provider::ZoneHeader> {
        self.node.provider.latest_header(crate::network::ZONE).await?.ok_or_else(|| CoreError::Network("no head".into()))
    }

    async fn block_hash(&self, block: u64) -> Result<Option<String>> {
        Ok(self.node.provider.header_at(crate::network::ZONE, block).await?.map(|h| h.hash.to_string().to_lowercase()))
    }

    /// Messages-contract logs of one kind in `from..=to`, optionally from one address and under
    /// one tag, a page at a time.
    async fn board_logs(
        &self,
        contract: quai_sdk::QuaiAddress,
        kind: u8,
        from_address: Option<&str>,
        tag: Option<&[u8; 32]>,
        from: u64,
        to: u64,
    ) -> Result<Vec<Log>> {
        let one = |t: String| TopicMatch::AnyOf(t.parse().into_iter().collect());
        let topics = vec![
            one(crate::messages::MESSAGE_TOPIC.to_string()),
            from_address.map_or(TopicMatch::Any, |a| one(topic_of_address(a))),
            tag.map_or(TopicMatch::Any, |t| one(crate::messages::tag_topic(t))),
            one(topic_of_kind(kind)),
        ];
        let filter = LogFilter::new(crate::network::ZONE, LogRange::Inclusive { from, to })
            .with_addresses(vec![contract.into()])
            .with_topics(topics);
        Ok(self.node.provider.logs_served_through(&filter).await?.into_iter().filter(|l| !l.removed).collect())
    }

    async fn block_time(&self, block: u64, times: &mut HashMap<u64, u64>) -> u64 {
        if let Some(t) = times.get(&block) {
            return *t;
        }
        let t = self
            .node
            .provider
            .header_at(crate::network::ZONE, block)
            .await
            .ok()
            .flatten()
            .as_ref()
            .and_then(crate::network::header_time)
            .unwrap_or(0);
        times.insert(block, t);
        t
    }

    /// Valid announcements by `owner` in `from..=to`, oldest first.
    async fn announcements(
        &self,
        ctx: &wire::Context,
        contract: quai_sdk::QuaiAddress,
        owner: &str,
        from: u64,
        to: u64,
        times: &mut HashMap<u64, u64>,
    ) -> Result<Vec<KnownKey>> {
        let owner_bytes = wire::address_bytes(owner).ok_or_else(|| CoreError::Invalid(format!("not an address: {owner}")))?;
        let mut out = Vec::new();
        let mut start = from;
        while start <= to {
            let end = to.min(start + LOG_PAGE - 1);
            for log in self.board_logs(contract, wire::KIND_KEYS, Some(owner), Some(&wire::keys_tag()), start, end).await? {
                let topics: Vec<String> = log.topics.iter().map(|t| t.to_string()).collect();
                let block = log.inclusion.block_number;
                let Some(post) = crate::messages::decode_message(
                    &topics,
                    &log.data.to_hex(),
                    0,
                    block,
                    &log.transaction_hash.to_string(),
                    log.log_index,
                ) else {
                    continue;
                };
                if !post.from.eq_ignore_ascii_case(owner) {
                    continue;
                }
                if let Some(a) = wire::Announcement::verify(ctx, &owner_bytes, &post.body) {
                    let at = self.block_time(block, times).await;
                    out.push(KnownKey { sequence: a.sequence, week: a.week, identity: a.identity, weekly: a.weekly, block, at });
                }
            }
            start = end + 1;
        }
        Ok(out)
    }

    /// How far back a key is searched for from `from`: four weeks, and never before v3 existed on
    /// this network.
    fn key_floor(&self, from: u64) -> u64 {
        from.saturating_sub(KEY_LOOKBACK).max(self.network.ecosystem.messages_v3_from.unwrap_or(0)).min(from)
    }

    /// Read a peer's announcements into their record: backwards from `head` until one turns up
    /// the first time, forwards from where it stopped afterwards. The first identity seen is
    /// pinned; a different one later is held for the user as an identity change.
    async fn refresh_keys(
        &self,
        ctx: &wire::Context,
        contract: quai_sdk::QuaiAddress,
        peer: &mut PeerRecord,
        head: u64,
        times: &mut HashMap<u64, u64>,
    ) -> Result<()> {
        let mut found = Vec::new();
        if peer.keys_to == 0 {
            let floor = self.key_floor(head);
            let mut end = head;
            loop {
                let start = floor.max(end.saturating_sub(LOG_PAGE - 1));
                found = self.announcements(ctx, contract, &peer.address, start, end, times).await?;
                if !found.is_empty() || start <= floor {
                    break;
                }
                end = start - 1;
            }
        } else if head > peer.keys_to {
            found = self.announcements(ctx, contract, &peer.address, peer.keys_to + 1, head, times).await?;
        }
        merge_keys(peer, found);
        peer.keys_to = peer.keys_to.max(head);
        Ok(())
    }

    /// Find the announcement of one weekly key a peer used, searching back from `before`.
    async fn find_key(
        &self,
        ctx: &wire::Context,
        contract: quai_sdk::QuaiAddress,
        peer: &mut PeerRecord,
        weekly: &[u8; 32],
        before: u64,
        times: &mut HashMap<u64, u64>,
    ) -> Result<Option<KnownKey>> {
        if let Some(k) = peer.key_for(weekly) {
            return Ok(Some(k.clone()));
        }
        let floor = self.key_floor(before);
        let mut end = before;
        loop {
            let start = floor.max(end.saturating_sub(LOG_PAGE - 1));
            let found = self.announcements(ctx, contract, &peer.address, start, end, times).await?;
            let hit = found.iter().any(|k| &k.weekly == weekly);
            merge_keys(peer, found);
            if hit || start <= floor {
                break;
            }
            end = start - 1;
        }
        Ok(peer.key_for(weekly).cloned())
    }

    // --------------------------------------------------------------------- keys

    /// Where messaging stands for the account messages go from. Reads the chain for its own
    /// announcements.
    pub async fn messaging_status(&self) -> Result<Status> {
        let account = self.messaging_account();
        let mut status =
            Status { account: account.clone(), need: KeyNeed::NoKeys, fingerprint: None, key: None, keys_held: 0, scanned_to: None };
        let Some(account) = account else { return Ok(status) };
        let Some(opened) = self.open_key_file()? else { return Ok(status) };
        let file = &opened.file;
        let owner = wire::address_bytes(&account).ok_or_else(|| CoreError::Invalid("account".into()))?;
        status.fingerprint = Some(wire::fingerprint(&file.identity_public(), &owner));
        status.keys_held = file.weekly.len();
        let store = self.open_store(file)?;
        status.scanned_to = store.scanned()?.map(|(b, _)| b);
        status.need = self.key_need(file, &store).await?;
        let on_chain = matches!(status.need, KeyNeed::Ready);
        status.key = file.newest().map(|k| (k.sequence, k.week, on_chain));
        Ok(status)
    }

    /// Whether this week's key is published, reading this account's own announcements.
    async fn key_need(&self, file: &KeyFile, store: &Store) -> Result<KeyNeed> {
        let Some(newest) = file.newest() else { return Ok(KeyNeed::Publish) };
        if newest.week < wire::week_of(now()) {
            return Ok(KeyNeed::Publish);
        }
        let (ctx, contract) = self.messaging_context().await?;
        let head = self.head_header().await?.number;
        let mut own = own_record(store, file, head)?;
        let mut times = HashMap::new();
        self.refresh_keys(&ctx, contract, &mut own, head, &mut times).await?;
        store.put_peer(&own)?;
        if own.key_for(&newest.public).is_some() {
            return Ok(KeyNeed::Ready);
        }
        let weekly = hex::encode(newest.public);
        let in_flight = self.app.open_operations(&self.network.id)?.into_iter().any(|op| {
            op.detail.messaging() == "keys"
                && *op.detail.weekly() == weekly
                && matches!(op.status, OpStatus::Signed | OpStatus::Submitted | OpStatus::Unknown)
        });
        Ok(if in_flight { KeyNeed::Publishing } else { KeyNeed::Publish })
    }

    /// Review publishing this week's key (making it first if the week has none yet, and the
    /// account's messaging identity if this computer has none for it).
    pub async fn review_messaging_keys(&mut self, max_fee: Option<&str>) -> Result<Review> {
        let account = self.require_messaging_account()?;
        let (ctx, _) = self.messaging_context().await?;
        let new_identity = self.ensure_key_file().await?;
        let week = wire::week_of(now());
        let (announcement, fingerprint) = self.with_key_file(|f| {
            f.ensure_week(week)?;
            let public = f.newest().map(|k| k.public).ok_or_else(|| CoreError::Invalid("no weekly key".into()))?;
            let owner = wire::address_bytes(&f.account).ok_or_else(|| CoreError::Invalid("account".into()))?;
            Ok((f.announcement(&ctx, &public)?, wire::fingerprint(&f.identity_public(), &owner)))
        })?;
        let body = announcement.encode();
        let mut warnings = vec![
            "anyone can see that this address uses messaging; the key itself reveals nothing else".to_string(),
            "one key a week, only in weeks you use messaging; older keys are deleted two weeks after this one lands".into(),
        ];
        if new_identity {
            warnings.push(
                "this starts this account's messaging identity on this computer: if it messaged before, from another computer or before a restore, people will see a new identity"
                    .into(),
            );
        }
        self.review_board(
            Some(&account),
            &wire::keys_tag(),
            wire::KIND_KEYS,
            body,
            "messaging keys".into(),
            vec![
                field("Publishes", format!("this week's messaging key (week {}, #{})", announcement.week, announcement.sequence)),
                field("Your fingerprint", fingerprint),
            ],
            warnings,
            serde_json::json!({"messaging": "keys", "weekly": hex::encode(announcement.weekly), "sequence": announcement.sequence}),
            max_fee,
        )
        .await
    }

    // --------------------------------------------------------------------- peers

    /// A peer by contact name or messaging address, lowercase.
    pub fn resolve_peer(&self, peer: &str) -> Result<String> {
        let p = peer.trim();
        if p.starts_with("0x") && wire::address_bytes(p).is_some() {
            return Ok(p.to_lowercase());
        }
        let contacts = self.app.contacts()?;
        let contact = contacts
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(p))
            .ok_or_else(|| CoreError::NotFound(format!("`{p}` is neither an address nor a contact")))?;
        contact
            .address
            .as_deref()
            .filter(|a| a.starts_with("0x") && wire::address_bytes(a).is_some())
            .map(str::to_lowercase)
            .ok_or_else(|| CoreError::Invalid(format!("{} has no Quai address in the address book", contact.name)))
    }

    fn contact_name(&self, address: &str) -> Option<String> {
        self.app
            .contacts()
            .ok()?
            .into_iter()
            .find(|c| c.address.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(address)))
            .map(|c| c.name)
    }

    fn update_peer<R>(&self, peer: &str, f: impl FnOnce(&mut PeerRecord) -> Result<R>) -> Result<R> {
        let opened = self.require_key_file()?;
        let mut store = self.open_store(&opened.file)?;
        store.transaction(|s| {
            let mut p = s.peer(peer)?.unwrap_or_else(|| PeerRecord::new(peer, now()));
            let r = f(&mut p)?;
            s.put_peer(&p)?;
            Ok(r)
        })
    }

    /// Accept someone who wrote first; with a name, also into the address book.
    pub fn messaging_accept(&self, peer: &str, name: Option<&str>) -> Result<String> {
        let address = self.resolve_peer(peer)?;
        self.update_peer(&address, |p| {
            p.state = PeerState::Accepted;
            Ok(())
        })?;
        if let Some(name) = name.filter(|n| !n.trim().is_empty())
            && self.contact_name(&address).is_none()
        {
            self.app.add_contact(name.trim(), Some(&address), None, "messaging")?;
        }
        Ok(address)
    }

    /// Block (or unblock) an address: its messages are dropped unread from now on.
    pub fn messaging_block(&self, peer: &str, blocked: bool) -> Result<String> {
        let address = self.resolve_peer(peer)?;
        self.update_peer(&address, |p| {
            p.state = if blocked { PeerState::Blocked } else { PeerState::Request };
            Ok(())
        })?;
        Ok(address)
    }

    /// Accept a peer's new identity key. Sending to them resumes; the fingerprint must be
    /// compared again.
    pub fn messaging_trust(&self, peer: &str) -> Result<String> {
        let address = self.resolve_peer(peer)?;
        let owner = wire::address_bytes(&address).ok_or_else(|| CoreError::Invalid("address".into()))?;
        self.update_peer(&address, |p| {
            let new = p.changed_identity.take().ok_or_else(|| CoreError::Invalid("their identity has not changed".into()))?;
            p.identity = Some(new);
            p.verified = false;
            Ok(wire::fingerprint(&new, &owner))
        })
    }

    /// Both fingerprints, to compare in person or over another channel; `confirm` records that
    /// they matched.
    pub async fn messaging_verify(&self, peer: &str, confirm: bool) -> Result<Fingerprints> {
        let address = self.resolve_peer(peer)?;
        let opened = self.require_key_file()?;
        let own = wire::address_bytes(&opened.file.account).ok_or_else(|| CoreError::Invalid("account".into()))?;
        let ours = wire::fingerprint(&opened.file.identity_public(), &own);
        let (ctx, contract) = self.messaging_context().await?;
        let head = self.head_header().await?.number;
        let store = self.open_store(&opened.file)?;
        let mut p = store.peer(&address)?.unwrap_or_else(|| PeerRecord::new(&address, now()));
        let mut times = HashMap::new();
        self.refresh_keys(&ctx, contract, &mut p, head, &mut times).await?;
        let owner = wire::address_bytes(&address).ok_or_else(|| CoreError::Invalid("address".into()))?;
        let theirs = p.identity.map(|id| wire::fingerprint(&id, &owner));
        if confirm {
            if theirs.is_none() {
                return Err(CoreError::NotFound("they have not published messaging keys, so there is nothing to verify".into()));
            }
            if p.changed_identity.is_some() {
                return Err(CoreError::Invalid("their identity changed: accept it first (`message trust`), then verify".into()));
            }
            p.verified = true;
        }
        store.put_peer(&p)?;
        Ok(Fingerprints { peer: address, theirs, ours, verified: p.verified })
    }

    // ---------------------------------------------------------------------- send

    /// Review one sealed message. The text is in the review for the user to read once; the
    /// journal keeps neither it nor who it is for, and the local store keeps the text encrypted.
    pub async fn review_message(&mut self, peer: &str, text: &str, max_fee: Option<&str>) -> Result<Review> {
        let account = self.require_messaging_account()?;
        let address = self.resolve_peer(peer)?;
        if address == account {
            return Err(CoreError::Invalid("that is the account messages go from".into()));
        }
        let text = text.trim_end();
        let opened = self.require_key_file()?;
        let store = self.open_store(&opened.file)?;
        match self.key_need(&opened.file, &store).await? {
            KeyNeed::Ready | KeyNeed::Publishing => {}
            _ => return Err(CoreError::Invalid("publish this week's messaging key first: `quai-terminal message keys`".into())),
        }
        let (ctx, contract) = self.messaging_context().await?;
        let head = self.head_header().await?.number;
        let mut p = store.peer(&address)?.unwrap_or_else(|| PeerRecord::new(&address, now()));
        if p.state == PeerState::Blocked {
            return Err(CoreError::Invalid("you blocked this address; unblock it first".into()));
        }
        let mut times = HashMap::new();
        self.refresh_keys(&ctx, contract, &mut p, head, &mut times).await?;
        // Writing to someone is accepting them.
        p.state = PeerState::Accepted;
        store.put_peer(&p)?;
        if p.changed_identity.is_some() {
            return Err(CoreError::Invalid(
                "their identity key changed: compare fingerprints (`message verify`), then accept it (`message trust`)".into(),
            ));
        }
        let key = p.current_key().cloned().ok_or_else(|| {
            CoreError::NotFound(format!(
                "{} has not published a messaging key in the last four weeks; ask them to open their wallet",
                self.contact_name(&address).unwrap_or_else(|| crate::session::short_address(&address))
            ))
        })?;
        let newest = opened.file.newest().ok_or_else(|| CoreError::Invalid("no weekly key".into()))?;
        let sender = wire::address_bytes(&account).ok_or_else(|| CoreError::Invalid("account".into()))?;
        let recipient = wire::address_bytes(&address).ok_or_else(|| CoreError::Invalid("address".into()))?;
        let tag = wire::random_tag().ok_or_else(|| CoreError::Invalid("no randomness".into()))?;
        let envelope = wire::Envelope { ctx: &ctx, sender: &sender, recipient: &recipient, tag: &tag };
        let body = wire::seal(&envelope, &newest.secret(), &key.weekly, wire::CONTENT_TEXT, text.as_bytes()).map_err(|e| match e {
            wire::SealError::Empty => CoreError::Invalid("the message is empty".into()),
            wire::SealError::TooLong(n) => CoreError::Invalid(format!("a message is at most {} bytes; this one is {n}", wire::MAX_CONTENT)),
            _ => CoreError::Invalid("sealing the message failed".into()),
        })?;
        let name = self.contact_name(&address).unwrap_or_else(|| crate::session::short_address(&address));
        let fingerprint = wire::fingerprint(&key.identity, &recipient);
        let mut warnings = vec![
            "encrypted to them alone; on chain anyone sees the account it goes from, the time and the size (to a bucket), not who it is for"
                .into(),
            "it cannot be taken back once it is mined".into(),
        ];
        if !p.verified {
            warnings.push("you have not compared fingerprints with them (`message verify`)".into());
        }
        let age = now().saturating_sub(key.at);
        if key.at > 0 && age > STALE_KEY_SECS {
            warnings
                .push(format!("their newest key is {} days old: they may be away, and will read this when they are back", age / 86_400));
        }
        let review = self
            .review_board(
                Some(&account),
                &tag,
                wire::KIND_DM,
                body,
                "sealed message".into(),
                vec![
                    field("To", format!("{name} ({address})")),
                    field("Fingerprint", format!("{fingerprint}{}", if p.verified { " · verified" } else { "" })),
                    field("Message", text.to_string()),
                ],
                warnings,
                serde_json::json!({"messaging": "dm", "private_fields": PRIVATE_FIELDS}),
                max_fee,
            )
            .await?;
        // Kept before anything is signed: whatever happens next, this computer knows what it said.
        let record = MessageRecord {
            peer: address,
            outgoing: true,
            at: now(),
            content_type: wire::CONTENT_TEXT,
            text: text.to_string(),
            op_id: Some(review.op_id.clone()),
            ..Default::default()
        };
        store.put_message(&store.outgoing_id(&review.op_id), &record)?;
        Ok(review)
    }

    // ---------------------------------------------------------------------- read

    /// Read the chain for new messages to this wallet, and keep up with its own keys: a key whose
    /// successor is on chain is deleted after the grace period.
    pub async fn messaging_sync(&self) -> Result<SyncReport> {
        let opened = self.require_key_file()?;
        let mut store = self.open_store(&opened.file)?;
        let (ctx, contract) = self.messaging_context().await?;
        let head = self.head_header().await?;
        let mut times: HashMap<u64, u64> = HashMap::new();
        let mut report = SyncReport::default();
        let me = opened.file.account.clone();
        let me_bytes = wire::address_bytes(&me).ok_or_else(|| CoreError::Invalid("account".into()))?;

        // Our own announcements: the newest on chain supersedes the ones before it.
        let mut own = own_record(&store, &opened.file, head.number)?;
        self.refresh_keys(&ctx, contract, &mut own, head.number, &mut times).await?;
        store.put_peer(&own)?;
        let on_chain: Vec<[u8; 32]> = own.keys.iter().map(|k| k.weekly).collect();
        let deleted = self.with_key_file(|f| {
            if let Some(newest) = f.weekly.iter().rev().find(|k| on_chain.contains(&k.public)).map(|k| k.public) {
                f.announced(&newest, now());
            }
            Ok(f.prune(now()))
        })?;
        report.keys_deleted = deleted;
        let opened = self.require_key_file()?;
        let live = opened.file.live();
        let live: Vec<&wire::WeeklySecret> = live.iter().collect();

        // Where reading stopped, checked against the chain: a reorg behind it rewinds and drops
        // what was read from the blocks that are gone.
        let (mut cursor, hash) = store.scanned()?.unwrap_or((head.number, String::new()));
        if !hash.is_empty() && self.block_hash(cursor).await?.as_deref() != Some(hash.as_str()) {
            cursor = cursor.saturating_sub(REORG_REWIND);
            let stale: Vec<String> =
                store.messages()?.into_iter().filter(|(_, m)| !m.outgoing && m.block > cursor).map(|(id, _)| id).collect();
            for id in stale {
                store.delete_message(&id)?;
            }
        }
        let mut arrived: HashMap<String, usize> = HashMap::new();
        let mut start = cursor + 1;
        while start <= head.number {
            let end = head.number.min(start + LOG_PAGE - 1);
            let logs = self.board_logs(contract, wire::KIND_DM, None, None, start, end).await?;
            let mut accepted: Vec<(String, MessageRecord, PeerRecord)> = Vec::new();
            for log in logs {
                let topics: Vec<String> = log.topics.iter().map(|t| t.to_string()).collect();
                let block = log.inclusion.block_number;
                let tx = log.transaction_hash.to_string().to_lowercase();
                let Some(post) = crate::messages::decode_message(&topics, &log.data.to_hex(), 0, block, &tx, log.log_index) else {
                    continue;
                };
                if post.from.eq_ignore_ascii_case(&me) {
                    continue;
                }
                let id = store.chain_id(&tx, log.log_index);
                if store.has_message(&id)? {
                    continue;
                }
                let (Some(sender), Some(tag)) = (wire::address_bytes(&post.from), hex::decode(post.tag.trim_start_matches("0x")).ok())
                else {
                    continue;
                };
                let Ok(tag): std::result::Result<[u8; 32], _> = tag.try_into() else { continue };
                let envelope = wire::Envelope { ctx: &ctx, sender: &sender, recipient: &me_bytes, tag: &tag };
                let Some(opened_dm) = wire::open(&envelope, &post.body, &live) else { continue };
                let from = post.from.to_lowercase();
                let mut peer = match accepted.iter().rev().find(|(_, _, p)| p.address == from) {
                    Some((_, _, p)) => p.clone(),
                    None => store.peer(&from)?.unwrap_or_else(|| PeerRecord::new(&from, now())),
                };
                if peer.state == PeerState::Blocked {
                    report.blocked += 1;
                    continue;
                }
                // Believe the sender only if their address announced the key the body used.
                let Some(key) = self.find_key(&ctx, contract, &mut peer, &opened_dm.sender_weekly, block, &mut times).await? else {
                    report.rejected += 1;
                    continue;
                };
                let unverified = match peer.identity {
                    None => {
                        peer.identity = Some(key.identity);
                        false
                    }
                    Some(id) if id == key.identity => false,
                    Some(_) => {
                        peer.changed_identity = Some(key.identity);
                        true
                    }
                };
                let text = match opened_dm.content_type {
                    wire::CONTENT_TEXT => display_text(&opened_dm.content).unwrap_or_else(|| "(a message that is not text)".into()),
                    t => format!("(a kind of message this version cannot show: 0x{t:02x})"),
                };
                let at = self.block_time(block, &mut times).await;
                let record = MessageRecord {
                    peer: from.clone(),
                    outgoing: false,
                    at,
                    block,
                    block_hash: log.inclusion.block_hash.to_string().to_lowercase(),
                    tx: tx.clone(),
                    index: log.log_index,
                    content_type: opened_dm.content_type,
                    text,
                    op_id: None,
                    review_id: None,
                    unverified,
                };
                accepted.push((id, record, peer));
            }
            let end_hash = self.block_hash(end).await?.unwrap_or_default();
            store.transaction(|s| {
                for (id, record, peer) in &accepted {
                    s.put_message(id, record)?;
                    s.put_peer(peer)?;
                }
                s.set_scanned(end, &end_hash)
            })?;
            for (_, record, peer) in &accepted {
                if peer.state == PeerState::Accepted {
                    *arrived.entry(record.peer.clone()).or_default() += 1;
                } else {
                    report.requests += 1;
                }
            }
            start = end + 1;
        }
        report.arrived = arrived.into_iter().collect();
        report.arrived.sort();
        report.scanned_to = head.number.max(cursor);
        Ok(report)
    }

    /// Read the chain, then say which accepted conversations have messages nobody has read or
    /// been told about: (peer, how many). Each is said once, whichever process reads first, and
    /// requests are never said.
    pub async fn messaging_news(&self) -> Result<Vec<(String, usize)>> {
        self.messaging_sync().await?;
        let opened = self.require_key_file()?;
        let mut store = self.open_store(&opened.file)?;
        store.transaction(|s| {
            let messages = s.messages()?;
            let mut news = Vec::new();
            for mut p in s.peers()?.into_iter().filter(|p| p.state == PeerState::Accepted && p.address != opened.file.account) {
                let since = p.read_to.max(p.notified_to);
                let fresh: Vec<u64> =
                    messages.iter().map(|(_, m)| m).filter(|m| m.peer == p.address && !m.outgoing && m.at > since).map(|m| m.at).collect();
                if let Some(newest) = fresh.iter().max() {
                    news.push((p.address.clone(), fresh.len()));
                    p.notified_to = *newest;
                    s.put_peer(&p)?;
                }
            }
            Ok(news)
        })
    }

    /// Outgoing messages whose review was thrown away are forgotten; the rest say how far they got.
    fn outgoing_status(&self, store: &Store, id: &str, m: &MessageRecord) -> Result<Option<String>> {
        let Some(op_id) = &m.op_id else { return Ok(Some("sent".into())) };
        let status = self.app.operation(op_id)?.map(|op| op.status);
        let label = match status {
            None | Some(OpStatus::Cancelled) => None,
            Some(OpStatus::Prepared) if now().saturating_sub(m.at) > UNSENT_AFTER => None,
            Some(OpStatus::Prepared) => Some("not sent yet"),
            Some(OpStatus::Signed | OpStatus::Submitted | OpStatus::Unknown) => Some("pending"),
            Some(OpStatus::Failed | OpStatus::Replaced) => Some("failed"),
            Some(_) => Some("sent"),
        };
        if label.is_none() {
            store.delete_message(id)?;
        }
        Ok(label.map(str::to_string))
    }

    /// Everyone this wallet has messaged or heard from, newest first; `requests` lists those who
    /// wrote first and are waiting, instead of conversations.
    pub fn messaging_conversations(&self, requests: bool) -> Result<Vec<Conversation>> {
        let opened = self.require_key_file()?;
        let store = self.open_store(&opened.file)?;
        let messages = store.messages()?;
        let mut out = Vec::new();
        for p in store.peers()? {
            if p.address == opened.file.account {
                continue;
            }
            let wanted = if requests { p.state == PeerState::Request } else { p.state == PeerState::Accepted };
            if !wanted {
                continue;
            }
            let mine: Vec<&MessageRecord> = messages.iter().map(|(_, m)| m).filter(|m| m.peer == p.address).collect();
            if requests && mine.is_empty() {
                continue;
            }
            let owner = wire::address_bytes(&p.address);
            out.push(Conversation {
                peer: p.address.clone(),
                name: self.contact_name(&p.address),
                state: p.state,
                fingerprint: p.identity.zip(owner).map(|(id, o)| wire::fingerprint(&id, &o)),
                verified: p.verified,
                identity_changed: p.changed_identity.is_some(),
                messages: mine.len(),
                unread: mine.iter().filter(|m| !m.outgoing && m.at > p.read_to).count(),
                last_at: mine.iter().map(|m| m.at).max().unwrap_or(p.first_seen),
            });
        }
        out.sort_by(|a, b| b.last_at.cmp(&a.last_at));
        Ok(out)
    }

    /// One conversation, oldest first. `mark_read` records that the user has seen it.
    pub fn messaging_read(&self, peer: &str, mark_read: bool) -> Result<Vec<Line>> {
        let address = self.resolve_peer(peer)?;
        let opened = self.require_key_file()?;
        let store = self.open_store(&opened.file)?;
        let mut lines = Vec::new();
        for (id, m) in store.messages()?.into_iter().filter(|(_, m)| m.peer == address) {
            let status = if m.outgoing {
                match self.outgoing_status(&store, &id, &m)? {
                    Some(s) => s,
                    None => continue,
                }
            } else {
                "received".into()
            };
            lines.push(Line { at: m.at, outgoing: m.outgoing, text: m.text.clone(), status, unverified: m.unverified, tx: m.tx.clone() });
        }
        if mark_read && let Some(newest) = lines.iter().filter(|l| !l.outgoing).map(|l| l.at).max() {
            self.update_peer(&address, |p| {
                p.read_to = p.read_to.max(newest);
                Ok(())
            })?;
        }
        store.checkpoint();
        Ok(lines)
    }
}

/// This identity's own record. Its announcements cannot be older than its setup, so the first read
/// starts there instead of searching back weeks for a key that may not be published yet (which
/// took minutes on the public RPC). A store from before `origin` was kept estimates it from the
/// key file's age, with an hour to spare.
fn own_record(store: &Store, file: &KeyFile, head: u64) -> Result<PeerRecord> {
    if let Some(p) = store.peer(&file.account)? {
        return Ok(p);
    }
    let mut own = PeerRecord::new(&file.account, now());
    let origin = match store.origin()? {
        Some(block) => block,
        None => head.saturating_sub(now().saturating_sub(file.created) / BLOCK_SECS + 720),
    };
    // `keys_to` is where reading resumes from, so a fresh record reads forward from the origin.
    own.keys_to = origin.saturating_sub(1).max(1);
    Ok(own)
}

/// Add announcements to a peer's record, oldest first, without repeats, and pin or flag the
/// identity they carry.
fn merge_keys(peer: &mut PeerRecord, found: Vec<KnownKey>) {
    for k in found {
        if peer.keys.iter().any(|e| e.block == k.block && e.weekly == k.weekly) {
            continue;
        }
        peer.keys.push(k);
    }
    peer.keys.sort_by_key(|k| (k.block, k.sequence));
    // Only the newest keys matter for sending, and every key a message used is looked up again
    // when needed: a long history is not worth carrying.
    if peer.keys.len() > 64 {
        let drop = peer.keys.len() - 64;
        peer.keys.drain(..drop);
    }
    if let Some(newest) = peer.keys.last().map(|k| k.identity) {
        match peer.identity {
            None => peer.identity = Some(newest),
            Some(id) if id != newest && peer.changed_identity.is_none() => peer.changed_identity = Some(newest),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(block: u64, identity: u8, weekly: u8) -> KnownKey {
        KnownKey { sequence: block as u32, week: 1, identity: [identity; 32], weekly: [weekly; 32], block, at: block }
    }

    #[test]
    fn the_first_identity_is_pinned_and_a_new_one_is_held_for_the_user() {
        let mut p = PeerRecord::new("0xb0b", 0);
        merge_keys(&mut p, vec![key(10, 1, 1)]);
        assert_eq!((p.identity, p.changed_identity), (Some([1; 32]), None));
        merge_keys(&mut p, vec![key(20, 1, 2), key(10, 1, 1)]);
        assert_eq!(p.keys.len(), 2, "no repeats");
        assert_eq!(p.current_key().map(|k| k.weekly), Some([2; 32]));
        merge_keys(&mut p, vec![key(30, 9, 3)]);
        assert_eq!((p.identity, p.changed_identity), (Some([1; 32]), Some([9; 32])), "pinned stays, change waits");
        assert_eq!(p.current_key().map(|k| k.weekly), Some([2; 32]), "sending uses the pinned identity's newest key");
        assert_eq!(p.key_for(&[3; 32]).map(|k| k.identity), Some([9; 32]));
    }

    /// This identity's own announcements are read forward from its setup, never searched for
    /// weeks back: before any key is published that search found nothing, slowly.
    #[test]
    fn own_keys_are_read_from_the_setup_block() {
        let store = Store::memory([1; 32]).unwrap();
        let file = KeyFile::new("0x00a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1", now().saturating_sub(3600)).unwrap();
        // Without an origin, the key file's age says roughly where (an hour, plus an hour spare).
        let estimated = own_record(&store, &file, 10_000_000).unwrap();
        assert!(estimated.keys_to > 0, "never the backward search");
        assert!((10_000_000 - 720 - 720 - 2..=10_000_000 - 720 - 720).contains(&estimated.keys_to), "{}", estimated.keys_to);
        store.set_origin(9_999_000).unwrap();
        assert_eq!(own_record(&store, &file, 10_000_000).unwrap().keys_to, 9_998_999, "the setup block itself is read");
        let mut kept = PeerRecord::new(&file.account, 0);
        kept.keys_to = 10_000_500;
        store.put_peer(&kept).unwrap();
        assert_eq!(own_record(&store, &file, 10_001_000).unwrap().keys_to, 10_000_500, "a record already read resumes where it stopped");
    }

    #[test]
    fn text_is_shown_clean() {
        assert_eq!(display_text(b"gm\nall").as_deref(), Some("gm all"));
        assert_eq!(display_text(b"\x1b[31mred").as_deref().map(|s| s.contains('\x1b')), Some(false));
        assert_eq!(display_text(&[0xff, 0xfe]), None);
        assert_eq!(display_text(b"   "), None);
    }

    #[test]
    fn topics_are_left_padded() {
        assert_eq!(topic_of_kind(3), format!("0x{}03", "0".repeat(62)));
        assert_eq!(topic_of_address("0x00AbC"), format!("0x{}00abc", "0".repeat(59)));
    }
}
