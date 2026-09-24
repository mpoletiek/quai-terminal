//! The local message store, `messaging.sqlite`, beside the key file and like it never backed up.
//!
//! Every record is encrypted before SQLite sees it, so the file, its WAL and its free pages hold
//! no message text, no peer address and no transaction hash. A row is a kind, an opaque id (an
//! HMAC of what it names) and a sealed body; the only plain rows are how far the chain has been
//! read. What the file does show: how many records of each kind there are.

use crate::error::{CoreError, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use zeroize::Zeroizing;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS records(
  kind TEXT NOT NULL,
  id TEXT NOT NULL,
  body BLOB NOT NULL,
  PRIMARY KEY(kind, id)
);
CREATE TABLE IF NOT EXISTS progress(
  name TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
PRAGMA user_version=1;
";
const RECORD_LABEL: &[u8] = b"quai-terminal:messaging-store:v1";
const KIND_MESSAGE: &str = "m";
const KIND_PEER: &str = "p";

/// Where a peer stands with this wallet.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PeerState {
    /// Wrote first and has not been accepted: listed, never notified, text shown only on request.
    #[default]
    Request,
    /// A conversation.
    Accepted,
    /// Dropped before anything reaches the screen.
    Blocked,
}

/// A weekly key a peer announced, as read from the chain.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownKey {
    pub sequence: u32,
    pub week: u32,
    pub identity: [u8; 32],
    pub weekly: [u8; 32],
    pub block: u64,
    /// Unix seconds of that block.
    pub at: u64,
}

/// Someone this wallet has messaged or heard from.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerRecord {
    /// Their messaging address, lowercase.
    pub address: String,
    pub state: PeerState,
    /// The identity key first seen for them: the one every later announcement must carry.
    pub identity: Option<[u8; 32]>,
    /// A different identity key announced since, waiting for the user to accept it. Sending
    /// stops until they do; messages under it are held as unverified.
    pub changed_identity: Option<[u8; 32]>,
    /// The user compared fingerprints and said they match.
    pub verified: bool,
    /// Their announcements, oldest first.
    pub keys: Vec<KnownKey>,
    /// Announcements have been read up to this block.
    pub keys_to: u64,
    /// The newest message time the user has looked at.
    pub read_to: u64,
    /// The newest message time a notification has been made for.
    #[serde(default)]
    pub notified_to: u64,
    pub first_seen: u64,
}

impl PeerRecord {
    pub fn new(address: &str, now: u64) -> Self {
        Self { address: address.to_lowercase(), first_seen: now, ..Default::default() }
    }

    /// Their newest announced key under the pinned identity.
    pub fn current_key(&self) -> Option<&KnownKey> {
        self.keys.iter().rev().find(|k| Some(k.identity) == self.identity)
    }

    /// The announcement carrying this weekly key, whichever identity signed it.
    pub fn key_for(&self, weekly: &[u8; 32]) -> Option<&KnownKey> {
        self.keys.iter().rev().find(|k| &k.weekly == weekly)
    }
}

/// One message, either way.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageRecord {
    /// The other side's messaging address, lowercase.
    pub peer: String,
    pub outgoing: bool,
    /// Unix seconds: the block's time when read from the chain, the send time when outgoing.
    pub at: u64,
    /// Where it was read from the chain; zero for an outgoing message not yet seen there.
    pub block: u64,
    pub block_hash: String,
    pub tx: String,
    pub index: u64,
    pub content_type: u8,
    pub text: String,
    /// The operation that sent it (outgoing).
    pub op_id: Option<String>,
    /// The review it was prepared in (outgoing), until it is sent or dropped.
    pub review_id: Option<String>,
    /// Opened under an identity key the user has not accepted.
    pub unverified: bool,
}

/// The store, opened with its key.
pub struct Store {
    conn: Connection,
    key: Zeroizing<[u8; 32]>,
}

impl Store {
    pub fn open(path: &std::path::Path, key: Zeroizing<[u8; 32]>) -> Result<Self> {
        if let Some(dir) = path.parent() {
            crate::paths::ensure_private_dir(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let mode: String = conn.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            conn.pragma_update(None, "journal_mode", "WAL")?;
        }
        conn.pragma_update(None, "secure_delete", "ON")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 1 {
            return Err(CoreError::Storage("the message store is from a newer version".into()));
        }
        if version < 1 {
            conn.execute_batch(SCHEMA)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(Self { conn, key })
    }

    #[cfg(test)]
    pub fn memory(key: [u8; 32]) -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn, key: Zeroizing::new(key) })
    }

    /// An opaque id for what `name` names: nobody without the store key learns the address or
    /// transaction behind a row.
    fn id(&self, kind: &str, name: &str) -> String {
        use hmac::{Mac, SimpleHmac};
        let mut mac = <SimpleHmac<sha2::Sha256> as Mac>::new_from_slice(self.key.as_ref()).expect("any key length");
        mac.update(kind.as_bytes());
        mac.update(&[0]);
        mac.update(name.as_bytes());
        hex::encode(&mac.finalize().into_bytes()[..16])
    }

    fn aad(kind: &str, id: &str) -> Vec<u8> {
        [RECORD_LABEL, kind.as_bytes(), &[0], id.as_bytes()].concat()
    }

    fn seal<T: Serialize>(&self, kind: &str, id: &str, value: &T) -> Result<Vec<u8>> {
        use chacha20poly1305::{AeadInOut, KeyInit, XChaCha20Poly1305, XNonce};
        let mut plain = Zeroizing::new(serde_json::to_vec(value)?);
        let mut nonce = [0u8; 24];
        quai_sdk::crypto::fill_random(&mut nonce).map_err(|_| CoreError::Invalid("no randomness".into()))?;
        let cipher = XChaCha20Poly1305::new_from_slice(self.key.as_ref()).map_err(|_| CoreError::Invalid("store key".into()))?;
        let mac = cipher
            .encrypt_inout_detached(&XNonce::from(nonce), &Self::aad(kind, id), plain.as_mut_slice().into())
            .map_err(|_| CoreError::Invalid("sealing a record failed".into()))?;
        Ok([&nonce[..], plain.as_slice(), &mac[..]].concat())
    }

    fn unseal<T: DeserializeOwned>(&self, kind: &str, id: &str, body: &[u8]) -> Result<T> {
        use chacha20poly1305::{AeadInOut, KeyInit, Tag, XChaCha20Poly1305, XNonce};
        if body.len() < 24 + 16 {
            return Err(CoreError::Storage("a message store record is damaged".into()));
        }
        let (nonce, rest) = body.split_at(24);
        let (sealed, mac) = rest.split_at(rest.len() - 16);
        let cipher = XChaCha20Poly1305::new_from_slice(self.key.as_ref()).map_err(|_| CoreError::Invalid("store key".into()))?;
        let mut plain = Zeroizing::new(sealed.to_vec());
        let nonce: [u8; 24] = nonce.try_into().map_err(|_| CoreError::Storage("record".into()))?;
        let mac: [u8; 16] = mac.try_into().map_err(|_| CoreError::Storage("record".into()))?;
        cipher
            .decrypt_inout_detached(&XNonce::from(nonce), &Self::aad(kind, id), plain.as_mut_slice().into(), &Tag::from(mac))
            .map_err(|_| CoreError::Storage("a message store record does not open with this identity's key".into()))?;
        serde_json::from_slice(&plain).map_err(|_| CoreError::Storage("a message store record is damaged".into()))
    }

    fn put<T: Serialize>(&self, kind: &str, id: &str, value: &T) -> Result<()> {
        let body = self.seal(kind, id, value)?;
        self.conn.execute("INSERT OR REPLACE INTO records(kind,id,body) VALUES(?1,?2,?3)", params![kind, id, body])?;
        Ok(())
    }

    fn get<T: DeserializeOwned>(&self, kind: &str, id: &str) -> Result<Option<T>> {
        let body: Option<Vec<u8>> =
            self.conn.query_row("SELECT body FROM records WHERE kind=?1 AND id=?2", params![kind, id], |r| r.get(0)).optional()?;
        body.map(|b| self.unseal(kind, id, &b)).transpose()
    }

    fn all<T: DeserializeOwned>(&self, kind: &str) -> Result<Vec<(String, T)>> {
        let mut stmt = self.conn.prepare("SELECT id, body FROM records WHERE kind=?1")?;
        let rows = stmt.query_map([kind], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, body) = row?;
            out.push((id.clone(), self.unseal(kind, &id, &body)?));
        }
        Ok(out)
    }

    /// Run `f` in one immediate transaction: another process changing the store waits.
    pub fn transaction<R>(&mut self, f: impl FnOnce(&Store) -> Result<R>) -> Result<R> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        match f(self) {
            Ok(r) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(r)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    // ------------------------------------------------------------------- messages

    /// The id of a message read from the chain.
    pub fn chain_id(&self, tx: &str, index: u64) -> String {
        self.id(KIND_MESSAGE, &format!("{}:{index}", tx.to_lowercase()))
    }

    /// The id of an outgoing message, by the local id it was prepared under.
    pub fn outgoing_id(&self, local: &str) -> String {
        self.id(KIND_MESSAGE, &format!("out:{local}"))
    }

    pub fn put_message(&self, id: &str, m: &MessageRecord) -> Result<()> {
        self.put(KIND_MESSAGE, id, m)
    }

    pub fn message(&self, id: &str) -> Result<Option<MessageRecord>> {
        self.get(KIND_MESSAGE, id)
    }

    pub fn has_message(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM records WHERE kind=?1 AND id=?2", params![KIND_MESSAGE, id], |_| Ok(()))
            .optional()?
            .is_some())
    }

    pub fn delete_message(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM records WHERE kind=?1 AND id=?2", params![KIND_MESSAGE, id])?;
        Ok(())
    }

    /// Every message, oldest first.
    pub fn messages(&self) -> Result<Vec<(String, MessageRecord)>> {
        let mut all = self.all::<MessageRecord>(KIND_MESSAGE)?;
        all.sort_by(|a, b| (a.1.at, a.1.block, a.1.index).cmp(&(b.1.at, b.1.block, b.1.index)));
        Ok(all)
    }

    // ---------------------------------------------------------------------- peers

    pub fn peer(&self, address: &str) -> Result<Option<PeerRecord>> {
        self.get(KIND_PEER, &self.id(KIND_PEER, &address.to_lowercase()))
    }

    pub fn put_peer(&self, p: &PeerRecord) -> Result<()> {
        self.put(KIND_PEER, &self.id(KIND_PEER, &p.address.to_lowercase()), p)
    }

    pub fn peers(&self) -> Result<Vec<PeerRecord>> {
        Ok(self.all::<PeerRecord>(KIND_PEER)?.into_iter().map(|(_, p)| p).collect())
    }

    // ------------------------------------------------------------------- progress

    /// How far incoming messages have been read: the block and its hash.
    pub fn scanned(&self) -> Result<Option<(u64, String)>> {
        let v: Option<String> = self.conn.query_row("SELECT value FROM progress WHERE name='dm'", [], |r| r.get(0)).optional()?;
        Ok(v.and_then(|v| {
            let (block, hash) = v.split_once(':')?;
            Some((block.parse().ok()?, hash.to_string()))
        }))
    }

    pub fn set_scanned(&self, block: u64, hash: &str) -> Result<()> {
        self.conn.execute("INSERT OR REPLACE INTO progress(name,value) VALUES('dm',?1)", [format!("{block}:{hash}")])?;
        Ok(())
    }

    /// Fold the WAL into the file (after deleting text, so the old pages do not linger).
    pub fn checkpoint(&self) {
        let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip_and_ids_are_opaque() {
        let s = Store::memory([3; 32]).unwrap();
        let id = s.chain_id("0xABC", 2);
        assert_eq!(id, s.chain_id("0xabc", 2), "the same message is the same row");
        assert!(!id.contains("abc"));
        let m = MessageRecord { peer: "0xb0b".into(), text: "hello".into(), at: 5, ..Default::default() };
        s.put_message(&id, &m).unwrap();
        assert_eq!(s.message(&id).unwrap(), Some(m.clone()));
        assert!(s.has_message(&id).unwrap());
        let mut p = PeerRecord::new("0xB0B", 1);
        p.state = PeerState::Accepted;
        s.put_peer(&p).unwrap();
        assert_eq!(s.peer("0xb0b").unwrap().map(|p| p.state), Some(PeerState::Accepted));
        assert_eq!(s.peers().unwrap().len(), 1);
        s.delete_message(&id).unwrap();
        assert!(s.message(&id).unwrap().is_none());
        s.set_scanned(10, "0xhash").unwrap();
        assert_eq!(s.scanned().unwrap(), Some((10, "0xhash".into())));
    }

    /// A row copied under another id, or read with another identity's key, does not open.
    #[test]
    fn a_record_opens_only_where_it_was_written() {
        let s = Store::memory([3; 32]).unwrap();
        let m = MessageRecord { text: "hello".into(), ..Default::default() };
        let body = s.seal(KIND_MESSAGE, "a", &m).unwrap();
        assert!(s.unseal::<MessageRecord>(KIND_MESSAGE, "a", &body).is_ok());
        assert!(s.unseal::<MessageRecord>(KIND_MESSAGE, "b", &body).is_err(), "moved to another id");
        assert!(s.unseal::<MessageRecord>(KIND_PEER, "a", &body).is_err(), "moved to another kind");
        let other = Store::memory([4; 32]).unwrap();
        assert!(other.unseal::<MessageRecord>(KIND_MESSAGE, "a", &body).is_err(), "another key");
    }

    /// The point of encrypting records: nothing readable reaches the file or its log.
    #[test]
    fn the_file_and_its_log_hold_no_text_address_or_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messaging.sqlite");
        let s = Store::open(&path, Zeroizing::new([5; 32])).unwrap();
        let tx = "0x9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c5b4a39281706f5e4d3c2b1a0";
        let peer = "0x00b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0";
        let text = "the vault combination is 4-8-15-16";
        let m = MessageRecord { peer: peer.into(), tx: tx.into(), text: text.into(), ..Default::default() };
        s.put_message(&s.chain_id(tx, 0), &m).unwrap();
        s.put_peer(&PeerRecord::new(peer, 1)).unwrap();
        for file in ["messaging.sqlite", "messaging.sqlite-wal"] {
            let bytes = std::fs::read(dir.path().join(file)).unwrap_or_default();
            for needle in [text, &tx[2..], &peer[2..], "vault combination"] {
                assert!(!bytes.windows(needle.len()).any(|w| w == needle.as_bytes()), "{file} holds {needle}");
            }
        }
        drop(s);
        let s = Store::open(&path, Zeroizing::new([5; 32])).unwrap();
        assert_eq!(s.messages().unwrap()[0].1.text, text, "and it reads back");
    }
}
