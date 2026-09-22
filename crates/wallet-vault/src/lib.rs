//! Encrypted secret vault for Quai Terminal.
//!
//! The vault holds only secret key origins: the original mnemonic phrase (with its
//! language and optional BIP39 passphrase) and standalone imported private keys.
//! Everything else a wallet needs (addresses, xpubs, payment code) is public
//! metadata stored outside the vault so read-only features work while locked.
//!
//! Format: a small JSON envelope whose authenticated header binds the version,
//! Argon2id parameters, salt and nonce. The key is derived with Argon2id v1.3 and
//! the plaintext is sealed with XChaCha20-Poly1305 (both from the audited
//! RustCrypto implementations the Quai SDK already uses). No custom primitives.

use argon2::{Algorithm, Argon2, Params, Version};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use chacha20poly1305::{AeadInOut, KeyInit, Tag, XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

/// Envelope format identifier.
pub const FORMAT: &str = "quai-wallet-vault";
/// Current envelope version.
pub const VERSION: u32 = 1;
/// Minimum accepted vault password length (characters).
pub const MIN_PASSWORD_CHARS: usize = 8;
/// Largest plaintext a vault may contain; hostile files cannot force big allocations.
const MAX_PLAINTEXT: usize = 1 << 20;

/// Vault errors never contain secret material.
#[derive(Debug, Error)]
pub enum VaultError {
    /// Wrong password or modified/corrupted vault; indistinguishable by design.
    #[error("incorrect password or corrupted vault")]
    Authentication,
    /// The envelope is malformed or uses unsupported parameters.
    #[error("invalid vault file: {0}")]
    Format(&'static str),
    /// The password does not meet the minimum policy.
    #[error("password must be at least {MIN_PASSWORD_CHARS} characters")]
    WeakPassword,
    /// The operating system random number generator failed; nothing was written.
    #[error("operating system randomness unavailable")]
    Entropy,
    /// Filesystem failure while reading or atomically replacing the vault.
    #[error("vault I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Secrets could not be encoded or decoded.
    #[error("vault content could not be encoded")]
    Encoding,
}

/// Argon2id cost parameters persisted with each vault.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct KdfParams {
    /// Memory in KiB.
    pub memory_kib: u32,
    /// Iterations (passes).
    pub iterations: u32,
    /// Degree of parallelism (lanes).
    pub parallelism: u32,
}

impl KdfParams {
    /// Product default for new vaults: 256 MiB, t=3, p=4 — four times RFC 9106's
    /// memory-constrained profile, about 0.7 s on a 2026 desktop. Existing vaults keep their own
    /// parameters (the envelope records them) and move up when re-sealed, never down.
    pub const DEFAULT: KdfParams = KdfParams { memory_kib: 256 * 1024, iterations: 3, parallelism: 4 };

    /// Deliberately cheap parameters for unit tests and scripted development only.
    /// Accepted on open only when `allow_weak` is set, so production vaults cannot
    /// be silently downgraded by editing a file.
    pub const INSECURE_TEST: KdfParams = KdfParams { memory_kib: 256, iterations: 1, parallelism: 1 };

    fn validate(self, allow_weak: bool) -> Result<(), VaultError> {
        let strong = self.memory_kib >= 19 * 1024 && self.iterations >= 2;
        if (!strong && !allow_weak)
            || self.memory_kib > 4 * 1024 * 1024
            || self.iterations == 0
            || self.iterations > 64
            || self.parallelism == 0
            || self.parallelism > 16
        {
            return Err(VaultError::Format("unsupported key derivation parameters"));
        }
        Ok(())
    }
}

/// The original BIP39 recovery phrase and the inputs needed to reproduce the seed.
#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct MnemonicSecret {
    /// Space-separated phrase exactly as generated or imported.
    pub phrase: String,
    /// Wordlist language label (e.g. `english`).
    pub language: String,
    /// Optional BIP39 passphrase ("25th word"); empty when unused.
    pub passphrase: String,
}

/// Ledger a standalone imported private key belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Zeroize)]
#[serde(rename_all = "lowercase")]
pub enum KeyLedger {
    /// Quai account ledger key.
    Quai,
    /// Qi UTXO ledger key.
    Qi,
}

/// A standalone imported secp256k1 private key.
#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct ImportedKey {
    /// Lowercase hex address the key controls (public, used for lookup).
    pub address: String,
    /// Ledger of the address.
    pub ledger: KeyLedger,
    /// 32-byte scalar as lowercase hex without prefix.
    pub secret_hex: String,
}

/// Decrypted vault content.
#[derive(Clone, Default, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct Secrets {
    /// HD recovery phrase, absent for key-only wallets.
    pub mnemonic: Option<MnemonicSecret>,
    /// Imported standalone keys.
    pub imported: Vec<ImportedKey>,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secrets")
            .field("mnemonic", &self.mnemonic.as_ref().map(|_| "<redacted>"))
            .field("imported", &self.imported.len())
            .finish()
    }
}

/// On-disk vault envelope (public header plus ciphertext).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VaultFile {
    format: String,
    version: u32,
    kdf: KdfParams,
    salt: String,
    nonce: String,
    ciphertext: String,
}

fn random<const N: usize>() -> Result<[u8; N], VaultError> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|_| VaultError::Entropy)?;
    Ok(bytes)
}

fn check_password(password: &str) -> Result<(), VaultError> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(VaultError::WeakPassword);
    }
    Ok(())
}

fn derive_key(password: &str, salt: &[u8], params: KdfParams) -> Result<Zeroizing<[u8; 32]>, VaultError> {
    let argon_params = Params::new(params.memory_kib, params.iterations, params.parallelism, Some(32))
        .map_err(|_| VaultError::Format("unsupported key derivation parameters"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon_params);
    let mut key = Zeroizing::new([0u8; 32]);
    argon.hash_password_into(password.as_bytes(), salt, key.as_mut()).map_err(|_| VaultError::Format("key derivation failed"))?;
    Ok(key)
}

impl VaultFile {
    fn aad(&self) -> Vec<u8> {
        format!(
            "{}|{}|{}|{}|{}|{}|{}",
            self.format, self.version, self.kdf.memory_kib, self.kdf.iterations, self.kdf.parallelism, self.salt, self.nonce
        )
        .into_bytes()
    }

    /// Encrypt secrets under a password with fresh salt and nonce.
    pub fn seal(secrets: &Secrets, password: &str, kdf: KdfParams) -> Result<Self, VaultError> {
        let plaintext = Zeroizing::new(serde_json::to_vec(secrets).map_err(|_| VaultError::Encoding)?);
        Self::seal_bytes(&plaintext, password, kdf, MAX_PLAINTEXT)
    }

    /// Authenticate and decrypt arbitrary bytes sealed with [`VaultFile::seal_bytes`].
    pub fn open_bytes(&self, password: &str, allow_weak: bool, limit: usize) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        self.open_inner(password, allow_weak, limit)
    }

    /// Encrypt arbitrary bytes (e.g. an application backup archive) with a size limit.
    pub fn seal_bytes(bytes: &[u8], password: &str, kdf: KdfParams, limit: usize) -> Result<Self, VaultError> {
        check_password(password)?;
        kdf.validate(true)?;
        let plaintext = Zeroizing::new(bytes.to_vec());
        if plaintext.len() > limit {
            return Err(VaultError::Format("vault content too large"));
        }
        let salt: [u8; 16] = random()?;
        let nonce: [u8; 24] = random()?;
        let mut file = VaultFile {
            format: FORMAT.into(),
            version: VERSION,
            kdf,
            salt: B64.encode(salt),
            nonce: B64.encode(nonce),
            ciphertext: String::new(),
        };
        let key = derive_key(password, &salt, kdf)?;
        let cipher = XChaCha20Poly1305::new_from_slice(&key[..]).map_err(|_| VaultError::Encoding)?;
        let mut buffer = Zeroizing::new(plaintext.to_vec());
        let tag =
            cipher.encrypt_inout_detached(&XNonce::from(nonce), &file.aad(), (&mut buffer[..]).into()).map_err(|_| VaultError::Encoding)?;
        let mut sealed = buffer.to_vec();
        sealed.extend_from_slice(&tag);
        file.ciphertext = B64.encode(sealed);
        Ok(file)
    }

    /// Authenticate and decrypt. `allow_weak` permits test-only KDF parameters.
    pub fn open(&self, password: &str, allow_weak: bool) -> Result<Secrets, VaultError> {
        let plaintext = self.open_inner(password, allow_weak, MAX_PLAINTEXT)?;
        serde_json::from_slice(&plaintext).map_err(|_| VaultError::Authentication)
    }

    fn open_inner(&self, password: &str, allow_weak: bool, limit: usize) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        if self.format != FORMAT || self.version != VERSION {
            return Err(VaultError::Format("unsupported vault version"));
        }
        self.kdf.validate(allow_weak)?;
        let salt = B64.decode(&self.salt).map_err(|_| VaultError::Format("salt"))?;
        let nonce: [u8; 24] =
            B64.decode(&self.nonce).map_err(|_| VaultError::Format("nonce"))?.try_into().map_err(|_| VaultError::Format("nonce length"))?;
        if salt.len() != 16 {
            return Err(VaultError::Format("salt length"));
        }
        if self.ciphertext.len() > limit.saturating_mul(2).saturating_add(64) {
            return Err(VaultError::Format("vault content too large"));
        }
        let sealed = B64.decode(&self.ciphertext).map_err(|_| VaultError::Format("ciphertext"))?;
        if sealed.len() < 16 {
            return Err(VaultError::Authentication);
        }
        let key = derive_key(password, &salt, self.kdf)?;
        let cipher = XChaCha20Poly1305::new_from_slice(&key[..]).map_err(|_| VaultError::Encoding)?;
        let (body, tag) = sealed.split_at(sealed.len() - 16);
        let tag: [u8; 16] = tag.try_into().map_err(|_| VaultError::Authentication)?;
        let mut plaintext = Zeroizing::new(body.to_vec());
        cipher
            .decrypt_inout_detached(&XNonce::from(nonce), &self.aad(), (&mut plaintext[..]).into(), &Tag::from(tag))
            .map_err(|_| VaultError::Authentication)?;
        Ok(plaintext)
    }

    /// Parameters used by this vault.
    pub fn kdf(&self) -> KdfParams {
        self.kdf
    }

    /// Serialize the envelope.
    pub fn to_json(&self) -> Result<String, VaultError> {
        serde_json::to_string_pretty(self).map_err(|_| VaultError::Encoding)
    }

    /// Parse an envelope with the default (vault) size bound.
    pub fn from_json(text: &str) -> Result<Self, VaultError> {
        Self::from_json_limited(text, MAX_PLAINTEXT)
    }

    /// Parse an envelope whose plaintext may be up to `limit` bytes.
    pub fn from_json_limited(text: &str, limit: usize) -> Result<Self, VaultError> {
        if text.len() > limit.saturating_mul(3).saturating_add(4096) {
            return Err(VaultError::Format("vault file too large"));
        }
        serde_json::from_str(text).map_err(|_| VaultError::Format("malformed envelope"))
    }

    /// Read a vault from disk.
    pub fn read(path: &Path) -> Result<Self, VaultError> {
        Self::from_json(&std::fs::read_to_string(path)?)
    }

    /// Atomically write with owner-only permissions: private temp file, fsync, rename, fsync dir.
    pub fn write_atomic(&self, path: &Path) -> Result<(), VaultError> {
        write_private_atomic(path, self.to_json()?.as_bytes())
    }
}

/// Atomically replace `path` with `bytes`, creating the file owner-readable only.
pub fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<(), VaultError> {
    let dir = path.parent().ok_or(VaultError::Format("vault path has no parent"))?;
    std::fs::create_dir_all(dir)?;
    let suffix: [u8; 6] = random()?;
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("vault"),
        suffix.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        #[cfg(unix)]
        std::fs::File::open(dir)?.sync_all()?;
        Ok::<(), std::io::Error>(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(result?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Secrets {
        Secrets {
            mnemonic: Some(MnemonicSecret {
                phrase: "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about".into(),
                language: "english".into(),
                passphrase: "".into(),
            }),
            imported: vec![ImportedKey { address: "0x00".into(), ledger: KeyLedger::Qi, secret_hex: "11".repeat(32) }],
        }
    }

    #[test]
    fn roundtrip_and_fresh_randomness() {
        let a = VaultFile::seal(&sample(), "correct horse", KdfParams::INSECURE_TEST).unwrap();
        let b = VaultFile::seal(&sample(), "correct horse", KdfParams::INSECURE_TEST).unwrap();
        assert_ne!(a.salt, b.salt);
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ciphertext, b.ciphertext);
        let opened = a.open("correct horse", true).unwrap();
        assert_eq!(opened.mnemonic.as_ref().unwrap().phrase, sample().mnemonic.clone().unwrap().phrase);
        assert_eq!(opened.imported.len(), 1);
        assert_eq!(opened.imported[0].ledger, KeyLedger::Qi);
    }

    #[test]
    fn wrong_password_fails_without_detail() {
        let a = VaultFile::seal(&sample(), "correct horse", KdfParams::INSECURE_TEST).unwrap();
        assert!(matches!(a.open("wrong horse!", true), Err(VaultError::Authentication)));
    }

    #[test]
    fn header_and_ciphertext_tampering_detected() {
        let a = VaultFile::seal(&sample(), "correct horse", KdfParams::INSECURE_TEST).unwrap();
        let mut b = a.clone();
        b.kdf.iterations = 2;
        assert!(matches!(b.open("correct horse", true), Err(VaultError::Authentication)));
        let mut c = a.clone();
        let mut raw = B64.decode(&c.ciphertext).unwrap();
        raw[3] ^= 1;
        c.ciphertext = B64.encode(raw);
        assert!(matches!(c.open("correct horse", true), Err(VaultError::Authentication)));
        let mut d = a.clone();
        d.nonce = B64.encode([0u8; 24]);
        assert!(matches!(d.open("correct horse", true), Err(VaultError::Authentication)));
    }

    #[test]
    fn weak_params_rejected_in_production_mode() {
        let a = VaultFile::seal(&sample(), "correct horse", KdfParams::INSECURE_TEST).unwrap();
        assert!(matches!(a.open("correct horse", false), Err(VaultError::Format(_))));
        let mut hostile = a.clone();
        hostile.kdf.memory_kib = u32::MAX;
        assert!(matches!(hostile.open("correct horse", true), Err(VaultError::Format(_))));
    }

    #[test]
    fn short_password_rejected() {
        assert!(matches!(VaultFile::seal(&sample(), "short", KdfParams::INSECURE_TEST), Err(VaultError::WeakPassword)));
    }

    #[test]
    fn debug_redacts() {
        let text = format!("{:?}", sample());
        assert!(!text.contains("abandon"));
        assert!(!text.contains("1111"));
    }

    #[test]
    fn atomic_write_is_private_and_readable() {
        let dir = std::env::temp_dir().join(format!("qw-vault-test-{}", std::process::id()));
        let path = dir.join("vault.json");
        let a = VaultFile::seal(&sample(), "correct horse", KdfParams::INSECURE_TEST).unwrap();
        a.write_atomic(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let back = VaultFile::read(&path).unwrap();
        assert!(back.open("correct horse", true).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn default_params_are_strong() {
        assert!(KdfParams::DEFAULT.validate(false).is_ok());
    }
}
