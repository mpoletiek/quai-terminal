//! The messaging key file: the identity key, the weekly keys and the key that encrypts the local
//! message store, for one wallet on one network.
//!
//! **Never backed up.** It lives in `wallets/<id>/messaging/<network>/`, outside `wallet.json`,
//! `app.sqlite` and the network databases, which are all a backup copies. It is sealed under a
//! key derived from the messaging account's private key, so a backup (which holds that account)
//! still cannot open a key file it never contains. Losing the file means a new identity and no
//! history, which is the decision this design is built on: nothing can bring back a deleted key.
//!
//! Every key is random. None is derived from the seed, a signature or a password.

use super::wire::{self, IdentitySecret, WeeklySecret};
use crate::error::{CoreError, Result};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

/// How long a weekly key is kept after the key that replaces it is on chain: long enough for
/// messages already sent to the old key to land and be read.
pub const GRACE_SECS: u64 = 14 * 24 * 3600;

const FILE_MAGIC: &[u8; 5] = b"QTMK1";
const FILE_VERSION: u8 = 1;
const WRAP_SALT: &[u8] = b"quai-terminal:messaging-keys:v1";
/// A key file holds a handful of keys; anything near this is not one.
const FILE_LIMIT: usize = 64 * 1024;

/// One weekly key, as held.
#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct WeeklyKey {
    pub sequence: u32,
    pub week: u32,
    secret: [u8; 32],
    pub public: [u8; 32],
    /// When a newer key of ours was first seen on chain; the secret goes [`GRACE_SECS`] later.
    pub superseded_at: Option<u64>,
}

impl WeeklyKey {
    pub fn secret(&self) -> WeeklySecret {
        WeeklySecret::from_parts(self.secret, self.public)
    }
}

/// Everything secret about one messaging identity.
#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct KeyFile {
    version: u8,
    /// The messaging account, lowercase.
    pub account: String,
    identity: [u8; 32],
    store_key: [u8; 32],
    /// Oldest first.
    pub weekly: Vec<WeeklyKey>,
    /// The next announcement's sequence number; never reused, even after keys are deleted.
    pub next_sequence: u32,
    pub created: u64,
}

impl KeyFile {
    /// A new identity for `account`: a fresh identity key and store key, no weekly key yet.
    pub fn new(account: &str, now: u64) -> Result<Self> {
        let identity = IdentitySecret::generate().ok_or_else(no_randomness)?;
        let mut store_key = [0u8; 32];
        quai_sdk::crypto::fill_random(&mut store_key).map_err(|_| no_randomness())?;
        Ok(Self {
            version: FILE_VERSION,
            account: account.to_lowercase(),
            identity: *identity.seed(),
            store_key,
            weekly: Vec::new(),
            next_sequence: 1,
            created: now,
        })
    }

    pub fn identity(&self) -> IdentitySecret {
        IdentitySecret::from_seed(&self.identity)
    }

    pub fn identity_public(&self) -> [u8; 32] {
        self.identity().public()
    }

    pub(crate) fn store_key(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.store_key)
    }

    /// The newest weekly key, if any.
    pub fn newest(&self) -> Option<&WeeklyKey> {
        self.weekly.last()
    }

    /// This week's key: the newest when it is already this week's, otherwise a new one.
    /// Returns whether a key was created.
    pub fn ensure_week(&mut self, week: u32) -> Result<bool> {
        if self.newest().is_some_and(|k| k.week >= week) {
            return Ok(false);
        }
        let key = WeeklySecret::generate().ok_or_else(no_randomness)?;
        self.weekly.push(WeeklyKey {
            sequence: self.next_sequence,
            week,
            secret: *key.secret_bytes(),
            public: key.public(),
            superseded_at: None,
        });
        self.next_sequence += 1;
        Ok(true)
    }

    /// The key with this public half.
    pub fn key(&self, public: &[u8; 32]) -> Option<&WeeklyKey> {
        self.weekly.iter().find(|k| &k.public == public)
    }

    /// Every key still held, newest first: what an incoming message is tried against.
    pub fn live(&self) -> Vec<WeeklySecret> {
        self.weekly.iter().rev().map(WeeklyKey::secret).collect()
    }

    /// The key with public half `public` is on chain: every older key is superseded from `now`.
    /// Returns whether anything changed.
    pub fn announced(&mut self, public: &[u8; 32], now: u64) -> bool {
        let Some(sequence) = self.key(public).map(|k| k.sequence) else { return false };
        let mut changed = false;
        for k in self.weekly.iter_mut().filter(|k| k.sequence < sequence && k.superseded_at.is_none()) {
            k.superseded_at = Some(now);
            changed = true;
        }
        changed
    }

    /// Delete every key superseded more than [`GRACE_SECS`] ago. Returns how many went.
    pub fn prune(&mut self, now: u64) -> usize {
        let before = self.weekly.len();
        self.weekly.retain(|k| k.superseded_at.is_none_or(|at| now.saturating_sub(at) < GRACE_SECS));
        before - self.weekly.len()
    }

    /// The announcement for a held key, signed for `ctx`.
    pub fn announcement(&self, ctx: &wire::Context, public: &[u8; 32]) -> Result<wire::Announcement> {
        let key = self.key(public).ok_or_else(|| CoreError::NotFound("that weekly key is no longer held".into()))?;
        let owner = wire::address_bytes(&self.account).ok_or_else(|| CoreError::Invalid("messaging account".into()))?;
        Ok(wire::Announcement::sign(ctx, &owner, &self.identity(), key.week, key.sequence, key.public))
    }

    // ------------------------------------------------------------------------ file

    /// Seal for `path`, under `wrap` (see [`wrap_key`]), bound to the wallet and network.
    pub fn save(&self, path: &std::path::Path, wrap: &[u8; 32], binding: &str) -> Result<()> {
        use chacha20poly1305::{AeadInOut, KeyInit, XChaCha20Poly1305, XNonce};
        let mut plain = Zeroizing::new(serde_json::to_vec(self)?);
        let mut nonce = [0u8; 24];
        quai_sdk::crypto::fill_random(&mut nonce).map_err(|_| no_randomness())?;
        let cipher = XChaCha20Poly1305::new_from_slice(wrap).map_err(|_| CoreError::Invalid("key file key".into()))?;
        let mac = cipher
            .encrypt_inout_detached(&XNonce::from(nonce), &file_aad(binding), plain.as_mut_slice().into())
            .map_err(|_| CoreError::Invalid("sealing the key file failed".into()))?;
        let mut out = Vec::with_capacity(FILE_MAGIC.len() + 24 + plain.len() + 16);
        out.extend_from_slice(FILE_MAGIC);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&plain);
        out.extend_from_slice(&mac);
        if let Some(dir) = path.parent() {
            crate::paths::ensure_private_dir(dir)?;
        }
        wallet_vault::write_private_atomic(path, &out)?;
        Ok(())
    }

    /// Open the file at `path`; `Ok(None)` when there is none.
    pub fn load(path: &std::path::Path, wrap: &[u8; 32], binding: &str) -> Result<Option<Self>> {
        use chacha20poly1305::{AeadInOut, KeyInit, Tag, XChaCha20Poly1305, XNonce};
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if bytes.len() > FILE_LIMIT || bytes.len() < FILE_MAGIC.len() + 24 + 16 || &bytes[..FILE_MAGIC.len()] != FILE_MAGIC {
            return Err(CoreError::Storage("the messaging key file is not one this version reads".into()));
        }
        let (nonce, rest) = bytes[FILE_MAGIC.len()..].split_at(24);
        let (sealed, mac) = rest.split_at(rest.len() - 16);
        let cipher = XChaCha20Poly1305::new_from_slice(wrap).map_err(|_| CoreError::Invalid("key file key".into()))?;
        let mut plain = Zeroizing::new(sealed.to_vec());
        let nonce: [u8; 24] = nonce.try_into().map_err(|_| CoreError::Storage("key file".into()))?;
        let mac: [u8; 16] = mac.try_into().map_err(|_| CoreError::Storage("key file".into()))?;
        cipher
            .decrypt_inout_detached(&XNonce::from(nonce), &file_aad(binding), plain.as_mut_slice().into(), &Tag::from(mac))
            .map_err(|_| CoreError::Storage("the messaging key file does not open for this account; it belongs to another".into()))?;
        let file: KeyFile = serde_json::from_slice(&plain).map_err(|_| CoreError::Storage("the messaging key file is damaged".into()))?;
        if file.version != FILE_VERSION {
            return Err(CoreError::Storage("the messaging key file is from another version".into()));
        }
        Ok(Some(file))
    }
}

fn file_aad(binding: &str) -> Vec<u8> {
    [&FILE_MAGIC[..], binding.as_bytes()].concat()
}

fn no_randomness() -> CoreError {
    CoreError::Invalid("the operating system gave no randomness".into())
}

/// The key a key file is sealed under: HKDF over the messaging account's private key, bound to
/// the wallet, the network and the account. Whoever can spend from the account can open the file
/// on this disk; nobody can recreate the file's contents from it.
pub fn wrap_key(account_secret: &[u8; 32], binding: &str) -> Zeroizing<[u8; 32]> {
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(WRAP_SALT), account_secret);
    let mut key = Zeroizing::new([0u8; 32]);
    // Expanding 32 bytes from SHA-256 cannot fail.
    let _ = hk.expand(binding.as_bytes(), key.as_mut());
    key
}

/// What a key file is bound to: wallet, network and account.
pub fn binding(wallet: &str, network: &str, account: &str) -> String {
    format!("{wallet}\0{network}\0{}", account.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCOUNT: &str = "0x00A1a1A1a1a1A1a1a1A1a1a1A1a1A1a1A1a1A1a1";

    #[test]
    fn a_week_gets_one_key_and_sequences_never_repeat() {
        let mut f = KeyFile::new(ACCOUNT, 0).unwrap();
        assert!(f.ensure_week(2959).unwrap());
        assert!(!f.ensure_week(2959).unwrap(), "same week, same key");
        assert!(!f.ensure_week(2958).unwrap(), "an earlier week never makes a key");
        assert!(f.ensure_week(2961).unwrap());
        assert_eq!(f.weekly.iter().map(|k| (k.sequence, k.week)).collect::<Vec<_>>(), [(1, 2959), (2, 2961)]);
        // The older key is superseded once the newer one is on chain, and deleted after the grace.
        let newest = f.newest().unwrap().public;
        assert!(f.announced(&newest, 1000));
        assert!(!f.announced(&newest, 2000), "superseded once, from the first sighting");
        assert_eq!(f.prune(1000 + GRACE_SECS - 1), 0);
        assert_eq!(f.live().len(), 2);
        assert_eq!(f.prune(1000 + GRACE_SECS), 1);
        assert_eq!(f.live().iter().map(|k| k.public()).collect::<Vec<_>>(), [newest]);
        assert!(f.ensure_week(2962).unwrap());
        assert_eq!(f.newest().unwrap().sequence, 3, "sequence carries on after a deletion");
        assert_ne!(f.weekly[0].secret, f.weekly[1].secret);
    }

    #[test]
    fn the_file_opens_only_with_its_key_and_binding_and_holds_no_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messaging").join("keys.sealed");
        let mut f = KeyFile::new(ACCOUNT, 5).unwrap();
        f.ensure_week(2959).unwrap();
        let bind = binding("w1", "mainnet", ACCOUNT);
        let wrap = wrap_key(&[7; 32], &bind);
        f.save(&path, &wrap, &bind).unwrap();
        let raw = std::fs::read(&path).unwrap();
        for secret in [f.identity, f.store_key, f.weekly[0].secret] {
            assert!(!raw.windows(32).any(|w| w == secret), "a secret is in the file");
        }
        assert!(!raw.windows(8).any(|w| w == b"identity"), "field names are encrypted too");
        let back = KeyFile::load(&path, &wrap, &bind).unwrap().unwrap();
        assert_eq!((back.identity_public(), back.weekly[0].public), (f.identity_public(), f.weekly[0].public));
        assert!(KeyFile::load(&path, &wrap_key(&[8; 32], &bind), &bind).is_err(), "another account's key");
        let other = binding("w1", "orchard", ACCOUNT);
        assert!(KeyFile::load(&path, &wrap, &other).is_err(), "another network's binding");
        assert!(KeyFile::load(&dir.path().join("none"), &wrap, &bind).unwrap().is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0, "private to the user");
        }
    }

    #[test]
    fn an_announcement_is_signed_by_the_file_s_identity() {
        let mut f = KeyFile::new(ACCOUNT, 0).unwrap();
        f.ensure_week(2959).unwrap();
        let ctx = wire::Context { chain_id: 9, contract: [0x77; 20] };
        let public = f.newest().unwrap().public;
        let a = f.announcement(&ctx, &public).unwrap();
        let owner = wire::address_bytes(ACCOUNT).unwrap();
        assert_eq!(wire::Announcement::verify(&ctx, &owner, &a.encode()), Some(a.clone()));
        assert_eq!((a.identity, a.weekly, a.sequence, a.week), (f.identity_public(), public, 1, 2959));
    }
}
