//! Key origins: mnemonic generation/validation, public metadata derivation and unlocked signing material.

use crate::error::{CoreError, Result};
use crate::network::ZONE;
use quai_sdk::crypto::SecretKey;
use quai_sdk::payments::PrivatePaymentCode;
use quai_sdk::primitives::Address;
use quai_sdk::signer::LocalSigner;
use quai_sdk::wallet::qi_keys::QiKeyring;
use quai_sdk::wallet::{CoinType, HdWallet, Language, Mnemonic, Search};
use quai_sdk::{Ledger, U256};
use wallet_vault::{ImportedKey, KeyLedger, MnemonicSecret, Secrets};
use zeroize::Zeroizing;

/// Supported BIP39 wordlist names.
pub const LANGUAGES: &[&str] =
    &["english", "chinese-simplified", "chinese-traditional", "czech", "french", "italian", "japanese", "korean", "portuguese", "spanish"];

/// Map a wordlist name to the SDK language.
pub fn language(name: &str) -> Result<Language> {
    Ok(match name.to_ascii_lowercase().replace('_', "-").as_str() {
        "english" | "en" => Language::English,
        "chinese-simplified" | "zh-hans" => Language::SimplifiedChinese,
        "chinese-traditional" | "zh-hant" => Language::TraditionalChinese,
        "czech" | "cs" => Language::Czech,
        "french" | "fr" => Language::French,
        "italian" | "it" => Language::Italian,
        "japanese" | "ja" => Language::Japanese,
        "korean" | "ko" => Language::Korean,
        "portuguese" | "pt" => Language::Portuguese,
        "spanish" | "es" => Language::Spanish,
        other => {
            return Err(CoreError::Invalid(format!("unknown mnemonic language `{other}`; supported: {}", LANGUAGES.join(", "))));
        }
    })
}

/// Generate a new recovery phrase from fresh OS entropy (24 words by default).
pub fn generate_phrase(words: usize, lang: &str) -> Result<Zeroizing<String>> {
    if ![12, 15, 18, 21, 24].contains(&words) {
        return Err(CoreError::Invalid("word count must be 12, 15, 18, 21 or 24".into()));
    }
    let mnemonic =
        Mnemonic::generate(language(lang)?, words).map_err(|e| CoreError::Invalid(format!("mnemonic generation failed: {e}")))?;
    Ok(Zeroizing::new(mnemonic.phrase().expose().to_string()))
}

/// Normalize user-entered phrase whitespace (collapse runs, trim).
pub fn normalize_phrase(phrase: &str) -> Zeroizing<String> {
    Zeroizing::new(phrase.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Parse and checksum-validate a phrase.
pub fn parse_mnemonic(phrase: &str, lang: &str) -> Result<Mnemonic> {
    let normalized = normalize_phrase(phrase);
    Mnemonic::parse(language(lang)?, &normalized)
        .map_err(|_| CoreError::Invalid("invalid recovery phrase (unknown word or checksum mismatch)".into()))
}

/// Public identity derived from a mnemonic.
#[derive(Clone, Debug)]
pub struct HdPublic {
    /// Quai account 0 xpub (m/44'/994'/0').
    pub quai_xpub: String,
    /// Qi account 0 xpub (m/44'/969'/0').
    pub qi_xpub: String,
    /// BIP47 payment code for m/47'/969'/0'.
    pub payment_code: String,
    /// First Cyprus-1 Quai address (Pelagus's first account).
    pub first_quai: (u32, Address),
}

/// Derive the public identity of an HD wallet.
pub fn hd_public(secret: &MnemonicSecret) -> Result<HdPublic> {
    let mnemonic = parse_mnemonic(&secret.phrase, &secret.language)?;
    let quai = HdWallet::from_mnemonic(&mnemonic, &secret.passphrase, CoinType::Quai)?;
    let qi = HdWallet::from_mnemonic(&mnemonic, &secret.passphrase, CoinType::Qi)?;
    let seed = mnemonic.to_seed(&secret.passphrase);
    let payment = PrivatePaymentCode::from_seed(seed.expose(), 0)?;
    let first = quai.search(0, false, Search { zone: ZONE, start_index: 0, max_attempts: 100_000 }, || false)?;
    Ok(HdPublic {
        quai_xpub: quai.account_public(0)?.export(),
        qi_xpub: qi.account_public(0)?.export(),
        payment_code: payment.public_code().to_base58(),
        first_quai: (first.address.index, first.address.address),
    })
}

/// Parse a 32-byte hex private key (optional 0x prefix).
pub fn parse_secret_hex(text: &str) -> Result<SecretKey> {
    let trimmed = text.trim();
    let hex_part = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    if hex_part.len() != 64 {
        return Err(CoreError::Invalid("private key must be 32 bytes of hex".into()));
    }
    let mut bytes = Zeroizing::new([0u8; 32]);
    hex::decode_to_slice(hex_part, bytes.as_mut()).map_err(|_| CoreError::Invalid("private key must be hex".into()))?;
    SecretKey::from_bytes(&bytes).map_err(|_| CoreError::Invalid("invalid private key scalar".into()))
}

/// Classify an imported key by the ledger and zone of its address.
pub fn imported_key_record(key: &SecretKey) -> Result<ImportedKey> {
    let address = key.public_key().address();
    let ledger = match address.ledger() {
        Ledger::Quai => KeyLedger::Quai,
        Ledger::Qi => KeyLedger::Qi,
    };
    let zone = address.zone().map_err(|_| CoreError::Invalid("key address is not in a valid zone".into()))?;
    if zone != ZONE {
        return Err(CoreError::Invalid(format!("key address {address} is in zone {zone:?}; this release supports Cyprus-1 only")));
    }
    Ok(ImportedKey { address: address.to_string(), ledger, secret_hex: hex::encode(key_bytes(key)) })
}

fn key_bytes(key: &SecretKey) -> Zeroizing<[u8; 32]> {
    Zeroizing::new(*key.export_bytes().as_bytes())
}

/// Unlocked keys alive in this process (a test hook: custody keeps one per wallet).
static LIVE_UNLOCKED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How many sets of unlocked keys exist in this process right now.
pub fn live_unlocked() -> usize {
    LIVE_UNLOCKED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Decrypted signing material for an unlocked wallet. Dropping it clears the keys.
pub struct Unlocked {
    secrets: Secrets,
    pub(crate) vault_generation: Option<u64>,
    /// Quai HD root, when the wallet has a mnemonic.
    pub quai_hd: Option<HdWallet>,
    /// Qi HD root, when the wallet has a mnemonic.
    pub qi_hd: Option<HdWallet>,
    /// Private payment code, when the wallet has a mnemonic.
    pub payment: Option<PrivatePaymentCode>,
}

impl Drop for Unlocked {
    fn drop(&mut self) {
        LIVE_UNLOCKED.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl std::fmt::Debug for Unlocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Unlocked(<redacted>)")
    }
}

impl Unlocked {
    /// Build signing material from decrypted vault content.
    pub fn new(secrets: Secrets) -> Result<Self> {
        let (quai_hd, qi_hd, payment) = match &secrets.mnemonic {
            Some(m) => {
                let mnemonic = parse_mnemonic(&m.phrase, &m.language)?;
                let seed = mnemonic.to_seed(&m.passphrase);
                (
                    Some(HdWallet::from_seed(seed.expose(), CoinType::Quai)?),
                    Some(HdWallet::from_seed(seed.expose(), CoinType::Qi)?),
                    Some(PrivatePaymentCode::from_seed(seed.expose(), 0)?),
                )
            }
            None => (None, None, None),
        };
        LIVE_UNLOCKED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Self { secrets, quai_hd, qi_hd, payment, vault_generation: None })
    }

    /// The vault generation these keys were unlocked from.
    pub fn vault_generation(&self) -> Option<u64> {
        self.vault_generation
    }

    /// Decrypted secrets (for export and re-sealing).
    pub fn secrets(&self) -> &Secrets {
        &self.secrets
    }

    /// Mutable secrets for adding imported keys before re-sealing.
    pub fn secrets_mut(&mut self) -> &mut Secrets {
        &mut self.secrets
    }

    /// Secret key controlling a Quai address: HD child index or imported key.
    pub fn quai_key(&self, address: Address, hd_index: Option<u32>) -> Result<SecretKey> {
        if let Some(index) = hd_index {
            let hd = self.quai_hd.as_ref().ok_or_else(|| CoreError::Locked("wallet has no recovery phrase".into()))?;
            let key = hd.derive_key(0, false, index)?.secret_key()?;
            if key.public_key().address() != address {
                return Err(CoreError::Invalid("derived key does not match address".into()));
            }
            return Ok(key);
        }
        self.imported_key(address)
    }

    /// Imported secret key for an address.
    pub fn imported_key(&self, address: Address) -> Result<SecretKey> {
        let text = address.to_string().to_lowercase();
        let record = self
            .secrets
            .imported
            .iter()
            .find(|k| k.address.to_lowercase() == text)
            .ok_or_else(|| CoreError::NotFound(format!("no private key for {address}")))?;
        let key = parse_secret_hex(&record.secret_hex)?;
        if key.public_key().address() != address {
            return Err(CoreError::Invalid("imported key does not match address".into()));
        }
        Ok(key)
    }

    /// Local signer for a Quai address on a chain.
    pub fn quai_signer(&self, address: Address, hd_index: Option<u32>, chain_id: u64) -> Result<LocalSigner> {
        Ok(LocalSigner::new(self.quai_key(address, hd_index)?, U256::from(chain_id))?)
    }

    /// Qi keyring with the HD root and every imported Qi key loaded.
    pub fn qi_keyring(&self) -> Result<QiKeyring<'_>> {
        let mut ring = QiKeyring::new(self.qi_hd.as_ref())?;
        for record in self.secrets.imported.iter().filter(|k| k.ledger == KeyLedger::Qi) {
            ring.import(parse_secret_hex(&record.secret_hex)?)?;
        }
        Ok(ring)
    }

    /// Qi keyring that also resolves every registered payment-channel receive key.
    pub fn qi_keyring_with_channels(&self, store: &quai_sdk::wallet::storage::SqliteStore) -> Result<QiKeyring<'_>> {
        let mut ring = self.qi_keyring()?;
        if let Some(payment) = &self.payment {
            for channel in store.payment_channels(payment)? {
                ring.load_payment_channel(store, payment, channel.channel.counterparty_code())?;
            }
        }
        Ok(ring)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn generate_defaults_to_valid_24_words() {
        let phrase = generate_phrase(24, "english").unwrap();
        assert_eq!(phrase.split(' ').count(), 24);
        parse_mnemonic(&phrase, "english").unwrap();
        let other = generate_phrase(24, "english").unwrap();
        assert_ne!(*phrase, *other);
        assert!(generate_phrase(13, "english").is_err());
    }

    #[test]
    fn rejects_bad_checksum_and_normalizes_whitespace() {
        assert!(parse_mnemonic(&PHRASE.replace("about", "abandon"), "english").is_err());
        parse_mnemonic(&format!("  {}  ", PHRASE.replace(' ', "   ")), "english").unwrap();
    }

    #[test]
    fn public_identity_is_deterministic_and_matches_unlocked_keys() {
        let secret = MnemonicSecret { phrase: PHRASE.into(), language: "english".into(), passphrase: "".into() };
        let a = hd_public(&secret).unwrap();
        let b = hd_public(&secret).unwrap();
        assert_eq!(a.quai_xpub, b.quai_xpub);
        assert_eq!(a.payment_code, b.payment_code);
        assert_eq!(a.first_quai.1.zone().unwrap(), ZONE);
        assert_eq!(a.first_quai.1.ledger(), Ledger::Quai);
        let unlocked = Unlocked::new(Secrets { mnemonic: Some(secret.clone()), imported: vec![] }).unwrap();
        let key = unlocked.quai_key(a.first_quai.1, Some(a.first_quai.0)).unwrap();
        assert_eq!(key.public_key().address(), a.first_quai.1);
        // A passphrase selects a different wallet.
        let with_pass =
            hd_public(&MnemonicSecret { phrase: PHRASE.into(), language: "english".into(), passphrase: "TREZOR".into() }).unwrap();
        assert_ne!(with_pass.quai_xpub, a.quai_xpub);
    }

    #[test]
    fn private_key_parsing() {
        assert!(parse_secret_hex("0x12").is_err());
        assert!(parse_secret_hex(&"00".repeat(32)).is_err());
        let key = parse_secret_hex(&format!("0x{}", "01".repeat(32))).unwrap();
        let _ = key.public_key().address();
    }
}
