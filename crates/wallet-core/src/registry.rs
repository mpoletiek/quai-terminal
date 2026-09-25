//! Wallet registry: public metadata files, vault lifecycle, creation and import.

use crate::error::{CoreError, Result};
use crate::identity::{self, Unlocked};
use crate::network::ZONE;
use crate::paths::{Paths, ensure_private_dir, validate_id};
use quai_sdk::Ledger;
use quai_sdk::crypto::PublicKey;
use quai_sdk::primitives::Address;
use quai_sdk::wallet::{AccountPublic, CoinType, Search};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use wallet_vault::{KdfParams, KeyLedger, MnemonicSecret, Secrets, VaultFile};

/// Current wallet metadata format.
pub const META_VERSION: u32 = 2;

/// How the wallet holds keys.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WalletKind {
    /// Recovery phrase plus optional imported keys.
    Hd,
    /// Imported private keys only.
    Keys,
    /// Public addresses only; cannot sign.
    Watch,
}

/// A Quai ledger account (one address).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuaiAccount {
    /// Checksummed address.
    pub address: String,
    /// BIP44 child index under m/44'/994'/0'/0 when HD-derived.
    pub hd_index: Option<u32>,
    /// Compressed public key hex (for imported accounts; HD accounts rederive).
    #[serde(default)]
    pub public_key: Option<String>,
    /// User label.
    pub label: String,
    /// Hidden from normal lists (archived).
    #[serde(default)]
    pub archived: bool,
}

/// A standalone imported Qi key's public record.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct QiImported {
    /// Checksummed address.
    pub address: String,
    /// Compressed public key hex.
    pub public_key: String,
    /// User label.
    pub label: String,
}

/// A watch-only address.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WatchAddress {
    /// Checksummed address.
    pub address: String,
    /// User label.
    pub label: String,
}

/// Public wallet metadata. Contains no secrets.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletMeta {
    /// Format version.
    pub version: u32,
    /// Monotonically increasing committed wallet revision (legacy wallets start at zero).
    #[serde(default)]
    pub generation: u64,
    /// The generation at which the encrypted custody (the vault) last changed: a password change
    /// or an imported key. Keys unlocked before it must be unlocked again; keys unlocked before a
    /// public change (a new account, a label) are still this wallet's keys.
    #[serde(default)]
    pub custody_generation: u64,
    /// Stable random identifier.
    pub id: String,
    /// Unique display name.
    pub name: String,
    /// Creation time (unix seconds).
    pub created_at: u64,
    /// Key model.
    pub kind: WalletKind,
    /// Quai account 0 xpub.
    pub quai_xpub: Option<String>,
    /// Qi account 0 xpub.
    pub qi_xpub: Option<String>,
    /// BIP47 payment code (public).
    pub payment_code: Option<String>,
    /// Recovery phrase length, when HD.
    pub word_count: Option<usize>,
    /// Whether a BIP39 passphrase is in use.
    pub has_passphrase: bool,
    /// The user verified the recovery phrase backup.
    pub backed_up: bool,
    /// Quai accounts.
    pub quai_accounts: Vec<QuaiAccount>,
    /// Imported Qi keys.
    pub qi_imported: Vec<QiImported>,
    /// Watch-only addresses (both ledgers).
    pub watch: Vec<WatchAddress>,
    /// The account that acts when none is named (its address): what `@` chooses in the TUI and
    /// `account use` on the command line. Unset, or archived, means the first active account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_account: Option<String>,
}

impl WalletMeta {
    /// Addresses of all active Quai accounts and Quai watch-only addresses.
    pub fn quai_owner_addresses(&self) -> Vec<String> {
        let mut out: Vec<String> = self.quai_accounts.iter().filter(|a| !a.archived).map(|a| a.address.clone()).collect();
        for w in &self.watch {
            if w.address.parse::<Address>().is_ok_and(|a| a.ledger() == Ledger::Quai) {
                out.push(w.address.clone());
            }
        }
        out
    }

    /// Whether any signing key exists.
    pub fn can_sign(&self) -> bool {
        self.kind != WalletKind::Watch
    }

    /// Qi account public key, when HD.
    pub fn qi_account(&self) -> Result<Option<AccountPublic>> {
        self.qi_xpub.as_ref().map(|x| AccountPublic::import(x, CoinType::Qi, 0).map_err(CoreError::from)).transpose()
    }

    /// Quai account public key, when HD.
    pub fn quai_account(&self) -> Result<Option<AccountPublic>> {
        self.quai_xpub.as_ref().map(|x| AccountPublic::import(x, CoinType::Quai, 0).map_err(CoreError::from)).transpose()
    }

    /// Find a Quai account by address, label, or 1-based position.
    pub fn find_quai_account(&self, selector: &str) -> Result<&QuaiAccount> {
        let lower = selector.to_lowercase();
        let active: Vec<&QuaiAccount> = self.quai_accounts.iter().filter(|a| !a.archived).collect();
        if let Ok(n) = selector.parse::<usize>()
            && n >= 1
            && n <= active.len()
        {
            return Ok(active[n - 1]);
        }
        self.quai_accounts
            .iter()
            .find(|a| a.address.to_lowercase() == lower || a.label.to_lowercase() == lower)
            .ok_or_else(|| CoreError::NotFound(format!("no Quai account `{selector}`")))
    }

    /// Default (first active) Quai account.
    pub fn default_quai_account(&self) -> Result<&QuaiAccount> {
        let active = self.active_account.as_deref();
        self.quai_accounts
            .iter()
            .find(|a| !a.archived && active.is_some_and(|x| a.address.eq_ignore_ascii_case(x)))
            .or_else(|| self.quai_accounts.iter().find(|a| !a.archived))
            .ok_or_else(|| CoreError::NotFound("wallet has no Quai accounts".into()))
    }
}

/// Current unix time in seconds.
pub fn now() -> u64 {
    if let Some(at) = FROZEN.with(std::cell::Cell::get) {
        return at;
    }
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

thread_local! {
    static FROZEN: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Hold [`now`] at `at` on this thread only (`None` lets it run again). For snapshot tests,
/// whose screens are dated from now: other threads, and so other tests, keep the real clock.
#[doc(hidden)]
pub fn freeze_clock(at: Option<u64>) {
    FROZEN.with(|f| f.set(at));
}

fn random_id() -> Result<String> {
    let mut bytes = [0u8; 8];
    getrandom_fill(&mut bytes)?;
    Ok(hex::encode(bytes))
}

fn getrandom_fill(bytes: &mut [u8]) -> Result<()> {
    quai_sdk::crypto::fill_random(bytes).map_err(|_| CoreError::Storage("OS randomness unavailable".into()))
}

/// Wallet storage manager rooted at the data directory.
#[derive(Clone, Debug)]
pub struct Registry {
    paths: Paths,
    kdf: KdfParams,
    allow_weak_kdf: bool,
}

/// The journal contains public metadata and an already authenticated encrypted vault only.
/// Once durable, it is the authority until both compatibility files have been published.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Mutation {
    format: String,
    metadata: WalletMeta,
    vault: Option<VaultFile>,
}

/// A wrapper deliberately prevents pre-coordination binaries from opening upgraded metadata.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataFile {
    format: String,
    metadata: WalletMeta,
}

const MUTATION_FORMAT: &str = "quai-wallet-mutation-v1";
const METADATA_FORMAT: &str = "quai-wallet-metadata-v2";

impl Registry {
    /// Registry using production KDF parameters, or cheap parameters when
    /// `QUAI_TERMINAL_INSECURE_FAST_KDF=1` is set in a **debug build** (dev chains and tests; the
    /// older `QUAI_WALLET_` spelling still works). A release build ignores the variable: an
    /// environment someone else can set must never be able to weaken a vault.
    pub fn new(paths: Paths) -> Self {
        let fast = cfg!(debug_assertions)
            && ["QUAI_TERMINAL_INSECURE_FAST_KDF", "QUAI_WALLET_INSECURE_FAST_KDF"]
                .iter()
                .any(|name| std::env::var(name).is_ok_and(|v| v == "1"));
        Self { paths, kdf: if fast { KdfParams::INSECURE_TEST } else { KdfParams::DEFAULT }, allow_weak_kdf: fast }
    }

    /// Registry with cheap KDF parameters, for tests elsewhere in the crate.
    #[cfg(test)]
    pub(crate) fn fast(paths: Paths) -> Self {
        Self { paths, kdf: KdfParams::INSECURE_TEST, allow_weak_kdf: true }
    }

    /// Paths.
    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    fn meta_path(&self, id: &str) -> PathBuf {
        self.paths.wallet_dir(id).join("wallet.json")
    }

    fn vault_path(&self, id: &str) -> PathBuf {
        self.paths.wallet_dir(id).join("vault.json")
    }

    /// Stable kernel lock, shared by aliases of the same canonical registry directory. It is
    /// outside wallet directories so deleting/restoring a wallet cannot replace a held lock's inode.
    /// The registry-wide scope also serializes wallet names and new wallet installation.
    fn mutation_lock(&self) -> Result<File> {
        ensure_private_dir(self.paths.root())?;
        let path = self.paths.root().join(".wallet-mutations.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        file.lock()?;
        Ok(file)
    }

    fn journal_path(&self, id: &str) -> PathBuf {
        self.paths.wallet_dir(id).join("mutation.json")
    }

    fn read_meta(&self, id: &str) -> Result<WalletMeta> {
        validate_id(id)?;
        let text = std::fs::read_to_string(self.meta_path(id))?;
        let value: serde_json::Value = serde_json::from_str(&text)?;
        let meta = if value.get("format").is_some() {
            let file: MetadataFile = serde_json::from_value(value)?;
            if file.format != METADATA_FORMAT {
                return Err(CoreError::Storage("wallet metadata format is newer or unsupported".into()));
            }
            file.metadata
        } else {
            serde_json::from_value::<WalletMeta>(value)?
        };
        if meta.id != id || meta.version > META_VERSION {
            return Err(CoreError::Storage("wallet metadata identity/version mismatch".into()));
        }
        Ok(meta)
    }

    fn recover(&self, id: &str) -> Result<()> {
        validate_id(id)?;
        let path = self.journal_path(id);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let mutation: Mutation = serde_json::from_slice(&bytes)?;
        if mutation.format != MUTATION_FORMAT || mutation.metadata.id != id || mutation.metadata.version != META_VERSION {
            return Err(CoreError::Storage("wallet mutation identity/version mismatch; refusing recovery".into()));
        }
        if self.meta_path(id).exists() && self.read_meta(id)?.generation > mutation.metadata.generation {
            return Err(CoreError::Storage("wallet mutation is older than committed state; refusing recovery".into()));
        }
        if mutation.metadata.can_sign() != mutation.vault.is_some() {
            return Err(CoreError::Storage("wallet mutation lacks its encrypted custody state".into()));
        }
        if let Some(vault) = &mutation.vault {
            vault.write_atomic(&self.vault_path(id))?;
        }
        mutation_fault("vault")?;
        let file = MetadataFile { format: METADATA_FORMAT.into(), metadata: mutation.metadata };
        wallet_vault::write_private_atomic(&self.meta_path(id), serde_json::to_vec_pretty(&file)?.as_slice())?;
        mutation_fault("metadata")?;
        std::fs::remove_file(path)?;
        File::open(self.paths.wallet_dir(id))?.sync_all()?;
        Ok(())
    }

    fn commit(&self, meta: &mut WalletMeta, vault: Option<VaultFile>) -> Result<()> {
        validate_id(&meta.id)?;
        if meta.version > META_VERSION {
            return Err(CoreError::Storage("wallet metadata is from a newer version".into()));
        }
        if meta.can_sign() != vault.is_some() {
            return Err(CoreError::Storage("wallet metadata does not match its encrypted custody state".into()));
        }
        mutation_fault("before_journal")?;
        let mut next = meta.clone();
        next.version = META_VERSION;
        next.generation = next.generation.checked_add(1).ok_or_else(|| CoreError::Storage("wallet generation exhausted".into()))?;
        // Every commit rewrites the vault; only a different one changes custody.
        let custody_changed = match &vault {
            None => false,
            Some(v) => match VaultFile::read(&self.vault_path(&meta.id)) {
                Ok(current) => current.to_json()? != v.to_json()?,
                Err(_) => true,
            },
        };
        if custody_changed {
            next.custody_generation = next.generation;
        }
        let mutation = Mutation { format: MUTATION_FORMAT.into(), metadata: next.clone(), vault };
        wallet_vault::write_private_atomic(&self.journal_path(&meta.id), &serde_json::to_vec_pretty(&mutation)?)?;
        mutation_fault("journal")?;
        self.recover(&meta.id)?;
        // Persist newly created wallet/parent entries as well as the files inside the wallet.
        File::open(self.paths.wallets_dir())?.sync_all()?;
        File::open(self.paths.root())?.sync_all()?;
        *meta = next;
        Ok(())
    }

    fn current_vault(&self, meta: &WalletMeta) -> Result<Option<VaultFile>> {
        if meta.can_sign() { Ok(Some(VaultFile::read(&self.vault_path(&meta.id))?)) } else { Ok(None) }
    }

    fn load_locked(&self, id: &str) -> Result<WalletMeta> {
        self.recover(id)?;
        self.read_meta(id)
    }

    /// Reload authoritative metadata, completing any interrupted encrypted/public commit first.
    pub fn load(&self, id: &str) -> Result<WalletMeta> {
        let _lock = self.mutation_lock()?;
        self.load_locked(id)
    }

    /// All wallets sorted by creation time.
    pub fn list(&self) -> Result<Vec<WalletMeta>> {
        let _lock = self.mutation_lock()?;
        self.list_locked()
    }

    fn list_locked(&self) -> Result<Vec<WalletMeta>> {
        let entries = match std::fs::read_dir(self.paths.wallets_dir()) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            if entry.path().join("wallet.json").exists() || entry.path().join("mutation.json").exists() {
                let id = entry.file_name().into_string().map_err(|_| CoreError::Storage("invalid wallet directory name".into()))?;
                out.push(self.load_locked(&id)?);
            }
        }
        out.sort_by_key(|m| m.created_at);
        Ok(out)
    }

    /// Resolve a wallet by id or name; with no selector, the only wallet or the configured default.
    pub fn resolve(&self, selector: Option<&str>, default: Option<&str>) -> Result<WalletMeta> {
        let wallets = self.list()?;
        match selector.or(default) {
            Some(s) => {
                wallets.into_iter().find(|w| w.id == s || w.name == s).ok_or_else(|| CoreError::NotFound(format!("no wallet named `{s}`")))
            }
            None => match wallets.len() {
                0 => Err(CoreError::NotFound("no wallets yet; run `quai-terminal wallet create` or `wallet import`".into())),
                1 => Ok(wallets.into_iter().next().expect("one wallet")),
                _ => Err(CoreError::Invalid("several wallets exist; choose one with --wallet NAME".into())),
            },
        }
    }

    /// Save an explicit snapshot only if no other writer has changed its generation. Prefer
    /// `update_meta` for independent field edits, which merge into authoritative metadata.
    pub fn save(&self, meta: &mut WalletMeta) -> Result<()> {
        let _lock = self.mutation_lock()?;
        let current = self.load_locked(&meta.id)?;
        if current.generation != meta.generation {
            return Err(CoreError::Storage("wallet changed in another session; reload before saving".into()));
        }
        let vault = self.current_vault(&current)?;
        self.commit(meta, vault)
    }

    /// Apply a public metadata mutation to fresh state, publishing the caller's copy only after
    /// its durable commit. Callers must identify an account by address, not stale list position.
    pub fn update_meta<T>(&self, meta: &mut WalletMeta, update: impl FnOnce(&mut WalletMeta) -> Result<T>) -> Result<T> {
        let _lock = self.mutation_lock()?;
        let mut current = self.load_locked(&meta.id)?;
        let result = update(&mut current)?;
        if current.id != meta.id {
            return Err(CoreError::Invalid("a metadata update cannot change wallet identity".into()));
        }
        let vault = self.current_vault(&current)?;
        self.commit(&mut current, vault)?;
        *meta = current;
        Ok(result)
    }

    fn check_name(&self, name: &str) -> Result<()> {
        let trimmed = name.trim();
        if trimmed.is_empty() || trimmed.len() > 64 || trimmed.chars().any(char::is_control) {
            return Err(CoreError::Invalid("wallet name must be 1-64 printable characters".into()));
        }
        if self.list_locked()?.iter().any(|w| w.name == trimmed) {
            return Err(CoreError::Invalid(format!("a wallet named `{trimmed}` already exists")));
        }
        Ok(())
    }

    fn write_new(&self, meta: &WalletMeta, secrets: &Secrets, password: &str) -> Result<()> {
        let dir = self.paths.wallet_dir(&meta.id);
        if dir.exists() {
            return Err(CoreError::Storage("wallet directory already exists".into()));
        }
        ensure_private_dir(&dir)?;
        let result = (|| {
            let vault = VaultFile::seal(secrets, password, self.kdf)?;
            // Prove the vault opens before the encrypted recovery record becomes authoritative.
            vault.open(password, self.allow_weak_kdf)?;
            self.commit(&mut meta.clone(), Some(vault))
        })();
        if result.is_err() && !self.journal_path(&meta.id).exists() && !self.meta_path(&meta.id).exists() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        result
    }

    /// Create or import an HD wallet from a recovery phrase.
    pub fn create_hd(
        &self,
        name: &str,
        phrase: &str,
        language: &str,
        passphrase: &str,
        password: &str,
        verified_backup: bool,
    ) -> Result<WalletMeta> {
        let _lock = self.mutation_lock()?;
        self.check_name(name)?;
        let normalized = identity::normalize_phrase(phrase);
        let secret =
            MnemonicSecret { phrase: normalized.to_string(), language: language.to_ascii_lowercase(), passphrase: passphrase.to_string() };
        let public = identity::hd_public(&secret)?;
        if self.list_locked()?.iter().any(|w| w.quai_xpub.as_deref() == Some(public.quai_xpub.as_str())) {
            return Err(CoreError::Invalid("this recovery phrase (and passphrase) is already imported".into()));
        }
        let meta = WalletMeta {
            version: META_VERSION,
            generation: 0,
            custody_generation: 0,
            id: random_id()?,
            name: name.trim().to_string(),
            created_at: now(),
            kind: WalletKind::Hd,
            quai_xpub: Some(public.quai_xpub),
            qi_xpub: Some(public.qi_xpub),
            payment_code: Some(public.payment_code),
            word_count: Some(normalized.split(' ').count()),
            has_passphrase: !passphrase.is_empty(),
            backed_up: verified_backup,
            quai_accounts: vec![QuaiAccount {
                address: public.first_quai.1.to_string(),
                hd_index: Some(public.first_quai.0),
                public_key: None,
                label: "Account 1".into(),
                archived: false,
            }],
            qi_imported: vec![],
            watch: vec![],
            active_account: None,
        };
        let secrets = Secrets { mnemonic: Some(secret), imported: vec![] };
        self.write_new(&meta, &secrets, password)?;
        self.load_locked(&meta.id)
    }

    /// Create a key-only wallet from one imported private key.
    pub fn create_from_key(&self, name: &str, secret_hex: &str, password: &str) -> Result<WalletMeta> {
        let _lock = self.mutation_lock()?;
        self.check_name(name)?;
        let key = identity::parse_secret_hex(secret_hex)?;
        let record = identity::imported_key_record(&key)?;
        let mut meta = WalletMeta {
            version: META_VERSION,
            generation: 0,
            custody_generation: 0,
            id: random_id()?,
            name: name.trim().to_string(),
            created_at: now(),
            kind: WalletKind::Keys,
            quai_xpub: None,
            qi_xpub: None,
            payment_code: None,
            word_count: None,
            has_passphrase: false,
            backed_up: true,
            quai_accounts: vec![],
            qi_imported: vec![],
            watch: vec![],
            active_account: None,
        };
        add_public_record(&mut meta, &key, record.ledger, "Imported 1")?;
        let secrets = Secrets { mnemonic: None, imported: vec![record] };
        self.write_new(&meta, &secrets, password)?;
        self.load_locked(&meta.id)
    }

    /// Create a watch-only wallet.
    pub fn create_watch(&self, name: &str, addresses: &[(String, String)]) -> Result<WalletMeta> {
        let _lock = self.mutation_lock()?;
        self.check_name(name)?;
        if addresses.is_empty() {
            return Err(CoreError::Invalid("watch-only wallets need at least one address".into()));
        }
        let mut watch = Vec::new();
        for (address, label) in addresses {
            watch.push(WatchAddress { address: parse_any_address(address)?.to_string(), label: label.clone() });
        }
        let meta = WalletMeta {
            version: META_VERSION,
            generation: 0,
            custody_generation: 0,
            id: random_id()?,
            name: name.trim().to_string(),
            created_at: now(),
            kind: WalletKind::Watch,
            quai_xpub: None,
            qi_xpub: None,
            payment_code: None,
            word_count: None,
            has_passphrase: false,
            backed_up: true,
            quai_accounts: vec![],
            qi_imported: vec![],
            watch,
            active_account: None,
        };
        if self.paths.wallet_dir(&meta.id).exists() {
            return Err(CoreError::Storage("wallet directory already exists".into()));
        }
        ensure_private_dir(&self.paths.wallet_dir(&meta.id))?;
        let mut meta = meta;
        self.commit(&mut meta, None)?;
        Ok(meta)
    }

    /// Decrypt current custody state, after recovery, rather than trusting a session snapshot.
    pub fn unlock(&self, meta: &WalletMeta, password: &str) -> Result<Unlocked> {
        let _lock = self.mutation_lock()?;
        let current = self.load_locked(&meta.id)?;
        self.unlock_locked(&current, password)
    }

    /// Return a matching metadata/key snapshot under one lock.
    pub fn unlock_current(&self, id: &str, password: &str) -> Result<(WalletMeta, Unlocked)> {
        let _lock = self.mutation_lock()?;
        let current = self.load_locked(id)?;
        let unlocked = self.unlock_locked(&current, password)?;
        Ok((current, unlocked))
    }

    fn unlock_locked(&self, meta: &WalletMeta, password: &str) -> Result<Unlocked> {
        if meta.kind == WalletKind::Watch {
            return Err(CoreError::Locked("watch-only wallets have no keys".into()));
        }
        let vault = VaultFile::read(&self.vault_path(&meta.id))?;
        let secrets = vault.open(password, self.allow_weak_kdf).map_err(|e| match e {
            wallet_vault::VaultError::Authentication => CoreError::Locked("incorrect password".into()),
            other => other.into(),
        })?;
        let mut unlocked = Unlocked::new(secrets)?;
        unlocked.vault_generation = Some(meta.generation);
        Ok(unlocked)
    }

    fn seal_current(&self, meta: &WalletMeta, unlocked: &Unlocked, password: &str) -> Result<VaultFile> {
        let current = self.current_vault(meta)?.map(|v| v.kdf());
        let kdf = current.filter(|c| kdf_cost(*c) > kdf_cost(self.kdf)).unwrap_or(self.kdf);
        let vault = VaultFile::seal(unlocked.secrets(), password, kdf)?;
        vault.open(password, self.allow_weak_kdf)?;
        Ok(vault)
    }

    /// Import a key into authoritative custody, never into a stale decrypted session snapshot.
    pub fn add_key(
        &self,
        meta: &mut WalletMeta,
        unlocked: &mut Unlocked,
        password: &str,
        secret_hex: &str,
        label: &str,
    ) -> Result<Address> {
        let key = identity::parse_secret_hex(secret_hex)?;
        let record = identity::imported_key_record(&key)?;
        let address = key.public_key().address();
        let _lock = self.mutation_lock()?;
        let mut current = self.load_locked(&meta.id)?;
        let mut fresh = self.unlock_locked(&current, password)?;
        if fresh.secrets().imported.iter().any(|k| k.address.eq_ignore_ascii_case(&record.address))
            || current.quai_accounts.iter().any(|a| a.address.eq_ignore_ascii_case(&record.address))
        {
            return Err(CoreError::Invalid(format!("{address} is already in this wallet")));
        }
        let ledger = record.ledger;
        fresh.secrets_mut().imported.push(record);
        add_public_record(&mut current, &key, ledger, label)?;
        let vault = self.seal_current(&current, &fresh, password)?;
        self.commit(&mut current, Some(vault))?;
        fresh.vault_generation = Some(current.generation);
        *meta = current;
        *unlocked = fresh;
        Ok(address)
    }

    /// Change the password while holding the same lock as imports and all other wallet writers.
    pub fn change_password(&self, meta: &WalletMeta, old: &str, new: &str) -> Result<()> {
        let _lock = self.mutation_lock()?;
        let mut current = self.load_locked(&meta.id)?;
        let unlocked = self.unlock_locked(&current, old)?;
        let vault = self.seal_current(&current, &unlocked, new)?;
        self.commit(&mut current, Some(vault))
    }

    /// Rename a wallet without overwriting concurrent account or custody mutations.
    pub fn rename(&self, meta: &mut WalletMeta, name: &str) -> Result<()> {
        let _lock = self.mutation_lock()?;
        self.check_name(name)?;
        let mut current = self.load_locked(&meta.id)?;
        current.name = name.trim().to_string();
        let vault = self.current_vault(&current)?;
        self.commit(&mut current, vault)?;
        *meta = current;
        Ok(())
    }

    /// Permanently delete a wallet directory while excluding mutation/restore writers.
    pub fn delete(&self, meta: &WalletMeta) -> Result<()> {
        let _lock = self.mutation_lock()?;
        let current = self.load_locked(&meta.id)?;
        if current.generation != meta.generation {
            return Err(CoreError::Storage("wallet changed in another session; reload before deleting".into()));
        }
        std::fs::remove_dir_all(self.paths.wallet_dir(&meta.id))?;
        File::open(self.paths.wallets_dir())?.sync_all()?;
        Ok(())
    }

    /// Derive and record the next account from the current inventory under the mutation lock.
    pub fn add_quai_account(&self, meta: &mut WalletMeta, label: Option<&str>) -> Result<QuaiAccount> {
        self.update_meta(meta, |current| {
            let account =
                current.quai_account()?.ok_or_else(|| CoreError::Invalid("only recovery-phrase wallets can derive accounts".into()))?;
            let start = current.quai_accounts.iter().filter_map(|a| a.hd_index).max().map_or(0, |i| i + 1);
            let found = account.search(false, Search { zone: ZONE, start_index: start, max_attempts: 1_000_000 }, || false)?;
            let n = current.quai_accounts.iter().filter(|a| a.hd_index.is_some()).count() + 1;
            let record = QuaiAccount {
                address: found.address.address.to_string(),
                hd_index: Some(found.address.index),
                public_key: None,
                label: label.map_or_else(|| format!("Account {n}"), str::to_string),
                archived: false,
            };
            current.quai_accounts.push(record.clone());
            Ok(record)
        })
    }

    /// Record a discovery without replacing newer labels, keys or account entries.
    pub fn add_quai_account_at(&self, meta: &mut WalletMeta, index: u32, label: &str) -> Result<QuaiAccount> {
        self.update_meta(meta, |current| {
            let account =
                current.quai_account()?.ok_or_else(|| CoreError::Invalid("only recovery-phrase wallets can discover accounts".into()))?;
            let derived = account.derive_address(false, index)?;
            if derived.zone != ZONE || derived.address.ledger() != Ledger::Quai {
                return Err(CoreError::Invalid(format!("index {index} is not a Cyprus-1 Quai address")));
            }
            if let Some(existing) = current.quai_accounts.iter().find(|a| a.hd_index == Some(index)) {
                return Ok(existing.clone());
            }
            let record = QuaiAccount {
                address: derived.address.to_string(),
                hd_index: Some(index),
                public_key: None,
                label: label.into(),
                archived: false,
            };
            current.quai_accounts.push(record.clone());
            current.quai_accounts.sort_by_key(|a| (a.hd_index.is_none(), a.hd_index));
            Ok(record)
        })
    }

    /// Whether the vault uses deliberately weak development KDF parameters.
    pub fn insecure_kdf(&self) -> bool {
        self.allow_weak_kdf
    }

    /// KDF parameters used for new envelopes.
    pub fn kdf_params(&self) -> KdfParams {
        self.kdf
    }

    /// Install a restored wallet directory. Fails if the wallet id or name already exists.
    pub fn install_restored(&self, meta: &WalletMeta, vault_json: Option<&str>) -> Result<()> {
        let _lock = self.mutation_lock()?;
        validate_id(&meta.id)?;
        if meta.version > META_VERSION {
            return Err(CoreError::Storage("restored wallet metadata is from a newer version".into()));
        }
        if self.list_locked()?.iter().any(|w| w.id == meta.id || w.name == meta.name) {
            return Err(CoreError::Invalid(format!(
                "a wallet with id {} or name `{}` already exists; delete or rename it first",
                meta.id, meta.name
            )));
        }
        let dir = self.paths.wallet_dir(&meta.id);
        if dir.exists() {
            return Err(CoreError::Storage("wallet directory already exists".into()));
        }
        let vault = vault_json.map(VaultFile::from_json).transpose()?;
        if meta.can_sign() != vault.is_some() {
            return Err(CoreError::Storage("restored wallet metadata does not match its custody envelope".into()));
        }
        ensure_private_dir(&dir)?;
        let result = self.commit(&mut meta.clone(), vault);
        if result.is_err() && !self.journal_path(&meta.id).exists() && !self.meta_path(&meta.id).exists() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        result
    }

    /// Export one coherent committed public/encrypted snapshot. Exporters must not read the
    /// compatibility vault path separately from metadata during a concurrent mutation.
    pub fn encrypted_snapshot(&self, id: &str) -> Result<(WalletMeta, Option<String>)> {
        let _lock = self.mutation_lock()?;
        let meta = self.load_locked(id)?;
        let encrypted = self.current_vault(&meta)?.map(|v| v.to_json()).transpose()?;
        Ok((meta, encrypted))
    }

    /// Path of a wallet's encrypted vault.
    pub fn vault_file(&self, id: &str) -> PathBuf {
        self.vault_path(id)
    }
}

/// Storage-boundary failures are only available to in-module tests, never through environment
/// variables or production configuration.
fn mutation_fault(_boundary: &str) -> Result<()> {
    #[cfg(test)]
    if MUTATION_FAILURE.with(|f| f.borrow().as_deref() == Some(_boundary)) {
        return Err(CoreError::Storage(format!("injected mutation failure after {_boundary}")));
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    static MUTATION_FAILURE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Relative work of a key derivation: memory times passes.
fn kdf_cost(k: KdfParams) -> u64 {
    u64::from(k.memory_kib) * u64::from(k.iterations)
}

fn add_public_record(meta: &mut WalletMeta, key: &quai_sdk::crypto::SecretKey, ledger: KeyLedger, label: &str) -> Result<()> {
    let public: PublicKey = key.public_key();
    let address = public.address();
    let public_hex = hex::encode(public.to_compressed());
    match ledger {
        KeyLedger::Quai => meta.quai_accounts.push(QuaiAccount {
            address: address.to_string(),
            hd_index: None,
            public_key: Some(public_hex),
            label: label.to_string(),
            archived: false,
        }),
        KeyLedger::Qi => {
            meta.qi_imported.push(QiImported { address: address.to_string(), public_key: public_hex, label: label.to_string() })
        }
    }
    Ok(())
}

/// Parse an address of either ledger in Cyprus-1.
pub fn parse_any_address(text: &str) -> Result<Address> {
    let address: Address = text.trim().parse().map_err(|_| CoreError::Invalid(format!("invalid address `{text}`")))?;
    let zone = address.zone().map_err(|_| CoreError::Invalid(format!("address `{text}` is not in a known zone")))?;
    if zone != ZONE {
        return Err(CoreError::Invalid(format!("address `{text}` is in {zone:?}; this release supports Cyprus-1 only")));
    }
    Ok(address)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_account_that_acts_follows_the_choice_and_falls_back_to_the_first() {
        let account = |address: &str, archived: bool| QuaiAccount {
            address: address.into(),
            hd_index: None,
            public_key: None,
            label: address.into(),
            archived,
        };
        let mut meta = WalletMeta {
            version: META_VERSION,
            generation: 0,
            custody_generation: 0,
            id: "w".into(),
            name: "w".into(),
            created_at: 0,
            kind: WalletKind::Hd,
            quai_xpub: None,
            qi_xpub: None,
            payment_code: None,
            word_count: None,
            has_passphrase: false,
            backed_up: true,
            quai_accounts: vec![account("0xA", false), account("0xB", false), account("0xC", true)],
            qi_imported: vec![],
            watch: vec![],
            active_account: None,
        };
        assert_eq!(meta.default_quai_account().unwrap().address, "0xA", "nothing chosen: the first");
        meta.active_account = Some("0xb".into());
        assert_eq!(meta.default_quai_account().unwrap().address, "0xB", "the choice, whatever its case");
        meta.active_account = Some("0xC".into());
        assert_eq!(meta.default_quai_account().unwrap().address, "0xA", "an archived choice does not act");
        meta.active_account = Some("0xgone".into());
        assert_eq!(meta.default_quai_account().unwrap().address, "0xA", "an unknown choice does not act");
        // Older builds ignore the field; this one reads metadata written without it.
        let old = serde_json::to_value(&meta).unwrap();
        let mut stripped = old.clone();
        stripped.as_object_mut().unwrap().remove("active_account");
        let read: WalletMeta = serde_json::from_value(stripped).unwrap();
        assert_eq!(read.active_account, None);
    }

    use super::*;

    const PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn registry() -> (Registry, PathBuf) {
        let dir = std::env::temp_dir().join(format!("qw-registry-{}-{}", std::process::id(), random_id().unwrap()));
        let paths = Paths::resolve(Some(dir.clone())).unwrap();
        let mut r = Registry::new(paths);
        r.kdf = KdfParams::INSECURE_TEST;
        r.allow_weak_kdf = true;
        (r, dir)
    }

    #[test]
    fn create_unlock_accounts_and_keys() {
        let (reg, dir) = registry();
        let mut meta = reg.create_hd("main", PHRASE, "english", "", "password123", true).unwrap();
        assert_eq!(meta.quai_accounts.len(), 1);
        assert!(reg.create_hd("main", PHRASE, "english", "", "password123", true).is_err());
        assert!(reg.create_hd("dup", PHRASE, "english", "", "password123", true).is_err());
        assert!(matches!(reg.unlock(&meta, "wrongpass1"), Err(CoreError::Locked(_))));
        let mut unlocked = reg.unlock(&meta, "password123").unwrap();
        let second = reg.add_quai_account(&mut meta, None).unwrap();
        assert!(second.hd_index.unwrap() > meta.quai_accounts[0].hd_index.unwrap());
        let key = unlocked.quai_key(second.address.parse().unwrap(), second.hd_index).unwrap();
        assert_eq!(key.public_key().address().to_string(), second.address);
        // Import a Qi key: find a scalar whose address is Cyprus-1 Qi.
        let mut imported = None;
        for i in 1u32..20_000 {
            let hexkey = format!("{:064x}", i);
            let k = identity::parse_secret_hex(&hexkey).unwrap();
            let a = k.public_key().address();
            if a.ledger() == Ledger::Qi && a.zone().ok() == Some(ZONE) {
                imported = Some(hexkey);
                break;
            }
        }
        let hexkey = imported.unwrap();
        reg.add_key(&mut meta, &mut unlocked, "password123", &hexkey, "miner").unwrap();
        assert_eq!(meta.qi_imported.len(), 1);
        assert!(reg.add_key(&mut meta, &mut unlocked, "password123", &hexkey, "again").is_err());
        let reopened = reg.unlock(&meta, "password123").unwrap();
        assert_eq!(reopened.secrets().imported.len(), 1);
        reg.change_password(&meta, "password123", "newpassword9").unwrap();
        assert!(reg.unlock(&meta, "password123").is_err());
        assert!(reg.unlock(&meta, "newpassword9").is_ok());
        assert_eq!(reg.list().unwrap().len(), 1);
        assert_eq!(reg.resolve(None, None).unwrap().id, meta.id);
        let found = reg.list().unwrap()[0].clone();
        assert_eq!(found.quai_accounts.len(), 2);
        reg.delete(&reg.load(&meta.id).unwrap()).unwrap();
        assert!(reg.list().unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Re-sealing (a key import, a password change) never makes the vault cheaper to attack,
    /// even when this process was configured with cheaper development parameters.
    #[test]
    fn resealing_never_downgrades_the_key_derivation() {
        let (mut reg, dir) = registry();
        let strong = KdfParams { memory_kib: 19 * 1024, iterations: 2, parallelism: 1 };
        reg.kdf = strong;
        let meta = reg.create_hd("main", PHRASE, "english", "", "password123", true).unwrap();
        reg.kdf = KdfParams::INSECURE_TEST;
        reg.change_password(&meta, "password123", "newpassword9").unwrap();
        let kdf = VaultFile::read(&reg.vault_path(&meta.id)).unwrap().kdf();
        assert_eq!(kdf, strong, "kept the stronger parameters");
        assert!(reg.unlock(&meta, "newpassword9").is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn watch_only_cannot_unlock() {
        let (reg, dir) = registry();
        let unlocked = Unlocked::new(Secrets {
            mnemonic: Some(MnemonicSecret { phrase: PHRASE.into(), language: "english".into(), passphrase: "".into() }),
            imported: vec![],
        })
        .unwrap();
        let first = identity::hd_public(unlocked.secrets().mnemonic.as_ref().unwrap()).unwrap().first_quai.1;
        let meta = reg.create_watch("watch", &[(first.to_string(), "cold".into())]).unwrap();
        assert!(!meta.can_sign());
        assert!(reg.unlock(&meta, "whatever1").is_err());
        assert!(reg.create_watch("bad", &[("0x1234".into(), "x".into())]).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn imported_fixture_keys() -> Vec<String> {
        (1u32..20_000)
            .map(|n| format!("{n:064x}"))
            .filter(|s| {
                let address = identity::parse_secret_hex(s).unwrap().public_key().address();
                address.ledger() == Ledger::Qi && address.zone().ok() == Some(ZONE)
            })
            .take(2)
            .collect()
    }

    #[test]
    fn stale_imports_and_password_changes_preserve_authoritative_keys() {
        let (reg, dir) = registry();
        let mut a = reg.create_hd("main", PHRASE, "english", "", "password123", true).unwrap();
        let mut b = a.clone();
        let mut a_keys = reg.unlock(&a, "password123").unwrap();
        let mut b_keys = reg.unlock(&b, "password123").unwrap();
        let keys = imported_fixture_keys();
        let first = reg.add_key(&mut a, &mut a_keys, "password123", &keys[0], "first").unwrap();
        let second = reg.add_key(&mut b, &mut b_keys, "password123", &keys[1], "second").unwrap();
        assert_eq!(b_keys.secrets().imported.len(), 2);
        // A's metadata/key snapshot predates B, including for password rotation.
        reg.change_password(&a, "password123", "rotatedpassword").unwrap();
        assert!(reg.unlock(&a, "password123").is_err());
        let (_, keys) = reg.unlock_current(&a.id, "rotatedpassword").unwrap();
        for address in [first, second] {
            assert_eq!(keys.imported_key(address).unwrap().public_key().address(), address);
        }
        let before = reg.load(&a.id).unwrap();
        assert!(reg.add_key(&mut a, &mut a_keys, "password123", &imported_fixture_keys()[0], "old password").is_err());
        assert_eq!(reg.load(&a.id).unwrap().generation, before.generation);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A new account or a label is a public change: keys unlocked before it are still this
    /// wallet's keys. A new password (a new vault) is not: keys from before it must unlock again.
    #[test]
    fn only_a_custody_change_makes_unlocked_keys_stale() {
        let (reg, dir) = registry();
        let mut meta = reg.create_hd("main", PHRASE, "english", "", "password123", true).unwrap();
        let unlocked = reg.unlock(&meta, "password123").unwrap();
        let held = unlocked.vault_generation.unwrap();
        reg.add_quai_account(&mut meta, Some("messaging")).unwrap();
        reg.update_meta(&mut meta, |m| {
            m.backed_up = true;
            Ok(())
        })
        .unwrap();
        let current = reg.load(&meta.id).unwrap();
        assert!(current.generation > held, "public changes still move the generation");
        assert!(held >= current.custody_generation, "but not custody: the keys held stay good");
        reg.change_password(&meta, "password123", "rotatedpassword").unwrap();
        let current = reg.load(&meta.id).unwrap();
        assert!(held < current.custody_generation, "a new vault makes them stale");
        let (_, fresh) = reg.unlock_current(&meta.id, "rotatedpassword").unwrap();
        assert!(fresh.vault_generation.unwrap() >= current.custody_generation);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn key_handoff_keeps_its_origin_generation_after_public_metadata_refresh() {
        let (reg, dir) = registry();
        let mut meta = reg.create_hd("main", PHRASE, "english", "", "password123", true).unwrap();
        let unlocked = reg.unlock(&meta, "password123").unwrap();
        let copied = unlocked.duplicate().unwrap();
        let original = meta.generation;
        reg.change_password(&meta, "password123", "rotatedpassword").unwrap();
        reg.update_meta(&mut meta, |m| {
            m.backed_up = true;
            Ok(())
        })
        .unwrap();
        assert!(meta.generation > original);
        assert_eq!(copied.vault_generation, Some(original), "refreshing public state must not bless a stale key handoff");
        let (current, fresh) = reg.unlock_current(&meta.id, "rotatedpassword").unwrap();
        assert_eq!(fresh.vault_generation, Some(current.generation));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stale_public_updates_merge_and_snapshot_saves_fail_closed() {
        let (reg, dir) = registry();
        let mut a = reg.create_hd("main", PHRASE, "english", "", "password123", true).unwrap();
        let mut b = a.clone();
        let first = reg.add_quai_account(&mut a, None).unwrap();
        let second = reg.add_quai_account(&mut b, None).unwrap();
        assert_ne!(first.hd_index, second.hd_index);
        assert_eq!(b.quai_accounts.len(), 3);
        a.backed_up = false;
        assert!(reg.save(&mut a).is_err());
        reg.update_meta(&mut a, |current| {
            current.name = "renamed".into();
            Ok(())
        })
        .unwrap();
        assert_eq!(a.quai_accounts.len(), 3);
        assert!(a.backed_up);
        reg.update_meta(&mut b, |current| {
            current.quai_accounts.iter_mut().find(|x| x.address == first.address).unwrap().archived = true;
            Ok(())
        })
        .unwrap();
        assert_eq!(b.name, "renamed");
        assert_eq!(b.quai_accounts.len(), 3);
        assert!(reg.delete(&a).is_err(), "stale deletion cannot discard a newer generation");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_before_journal_keeps_prior_state_and_export_recovers_a_complete_snapshot() {
        let (reg, dir) = registry();
        let mut meta = reg.create_hd("main", PHRASE, "english", "", "password123", true).unwrap();
        let mut unlocked = reg.unlock(&meta, "password123").unwrap();
        let before = meta.generation;
        let keys = imported_fixture_keys();
        MUTATION_FAILURE.with(|f| *f.borrow_mut() = Some("before_journal".into()));
        assert!(reg.add_key(&mut meta, &mut unlocked, "password123", &keys[0], "new").is_err());
        MUTATION_FAILURE.with(|f| *f.borrow_mut() = None);
        assert_eq!(reg.load(&meta.id).unwrap().generation, before);
        assert!(unlocked.secrets().imported.is_empty());
        assert!(!reg.journal_path(&meta.id).exists());
        MUTATION_FAILURE.with(|f| *f.borrow_mut() = Some("vault".into()));
        assert!(reg.add_key(&mut meta, &mut unlocked, "password123", &keys[0], "new").is_err());
        MUTATION_FAILURE.with(|f| *f.borrow_mut() = None);
        let (exported, vault) = reg.encrypted_snapshot(&meta.id).unwrap();
        let secrets = VaultFile::from_json(&vault.unwrap()).unwrap().open("password123", true).unwrap();
        assert_eq!(exported.generation, before + 1);
        assert_eq!(exported.qi_imported.len(), secrets.imported.len());
        assert_eq!(secrets.imported.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn interrupted_commit_keeps_memory_staged_and_recovers_encrypted_state() {
        for boundary in ["journal", "vault", "metadata"] {
            let (reg, dir) = registry();
            let mut meta = reg.create_hd("main", PHRASE, "english", "", "password123", true).unwrap();
            let mut unlocked = reg.unlock(&meta, "password123").unwrap();
            let before = meta.generation;
            let keys = imported_fixture_keys();
            MUTATION_FAILURE.with(|f| *f.borrow_mut() = Some(boundary.into()));
            assert!(reg.add_key(&mut meta, &mut unlocked, "password123", &keys[0], "new").is_err());
            MUTATION_FAILURE.with(|f| *f.borrow_mut() = None);
            assert_eq!(meta.generation, before, "failed publication did not change session metadata");
            assert!(unlocked.secrets().imported.is_empty(), "failed publication did not mutate live secrets");
            let journal = std::fs::read_to_string(reg.journal_path(&meta.id)).unwrap();
            assert!(!journal.contains(PHRASE));
            assert!(!journal.contains(&keys[0]));
            assert!(!journal.contains("password123"));
            let (recovered, keys) = reg.unlock_current(&meta.id, "password123").unwrap();
            assert_eq!(recovered.generation, before + 1);
            assert_eq!(recovered.qi_imported.len(), 1);
            assert_eq!(keys.secrets().imported.len(), 1);
            assert!(!reg.journal_path(&meta.id).exists());
            assert_eq!(reg.load(&meta.id).unwrap().generation, recovered.generation, "recovery is idempotent");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn legacy_metadata_upgrades_and_newer_envelopes_fail_closed() {
        let (reg, dir) = registry();
        let mut meta = reg.create_hd("main", PHRASE, "english", "", "password123", true).unwrap();
        meta.version = 1;
        let mut flat = serde_json::to_value(&meta).unwrap();
        flat.as_object_mut().unwrap().remove("generation");
        wallet_vault::write_private_atomic(&reg.meta_path(&meta.id), &serde_json::to_vec(&flat).unwrap()).unwrap();
        let mut legacy = reg.load(&meta.id).unwrap();
        assert_eq!(legacy.generation, 0);
        reg.update_meta(&mut legacy, |m| {
            m.backed_up = true;
            Ok(())
        })
        .unwrap();
        let text = std::fs::read_to_string(reg.meta_path(&meta.id)).unwrap();
        assert!(serde_json::from_str::<WalletMeta>(&text).is_err(), "old flat-metadata readers cannot reopen an upgraded wallet");
        assert_eq!(legacy.version, META_VERSION);
        let mut future: serde_json::Value = serde_json::from_str(&text).unwrap();
        future["metadata"]["version"] = serde_json::json!(META_VERSION + 1);
        wallet_vault::write_private_atomic(&reg.meta_path(&meta.id), &serde_json::to_vec(&future).unwrap()).unwrap();
        assert!(reg.load(&meta.id).is_err());
        assert!(reg.unlock(&meta, "password123").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restore_before_commit_failure_is_retryable_without_losing_the_archive() {
        let (source, source_dir) = registry();
        let meta = source.create_hd("main", PHRASE, "english", "", "password123", true).unwrap();
        let (meta, vault) = source.encrypted_snapshot(&meta.id).unwrap();
        let (target, target_dir) = registry();
        MUTATION_FAILURE.with(|f| *f.borrow_mut() = Some("before_journal".into()));
        assert!(target.install_restored(&meta, vault.as_deref()).is_err());
        MUTATION_FAILURE.with(|f| *f.borrow_mut() = None);
        assert!(!target.paths.wallet_dir(&meta.id).exists());
        target.install_restored(&meta, vault.as_deref()).unwrap();
        let (current, keys) = target.unlock_current(&meta.id, "password123").unwrap();
        assert_eq!(current.quai_xpub, meta.quai_xpub);
        assert!(keys.secrets().mnemonic.is_some());
        assert!(target.install_restored(&meta, vault.as_deref()).is_err());
        std::fs::remove_dir_all(source_dir).unwrap();
        std::fs::remove_dir_all(target_dir).unwrap();
    }

    fn wait_for_file(path: &std::path::Path) {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !path.exists() {
            assert!(std::time::Instant::now() < until, "child synchronization timed out: {}", path.display());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn mutation_subprocess_worker() {
        let Ok(root) = std::env::var("QW_MUTATION_CHILD_ROOT") else { return };
        let mode = std::env::var("QW_MUTATION_CHILD_MODE").unwrap();
        let reg = Registry::fast(Paths::resolve(Some(root.into())).unwrap());
        let mut meta = reg.resolve(None, None).unwrap();
        let mut keys = reg.unlock(&meta, "password123").unwrap();
        let fixtures = imported_fixture_keys();
        if let Ok(index) = mode.parse::<usize>() {
            std::fs::write(reg.paths.root().join(format!("ready-{index}")), b"ready").unwrap();
            wait_for_file(&reg.paths.root().join("go"));
            let imported = reg.add_key(&mut meta, &mut keys, "password123", &fixtures[index], &format!("key-{index}"));
            if matches!(imported, Err(CoreError::Locked(_))) {
                reg.add_key(&mut meta, &mut keys, "rotatedpassword", &fixtures[index], &format!("key-{index}")).unwrap();
            } else {
                imported.unwrap();
            }
            reg.add_quai_account(&mut meta, None).unwrap();
        } else if mode == "rotate" {
            std::fs::write(reg.paths.root().join("ready-rotate"), b"ready").unwrap();
            wait_for_file(&reg.paths.root().join("go"));
            reg.change_password(&meta, "password123", "rotatedpassword").unwrap();
        } else {
            MUTATION_FAILURE.with(|f| *f.borrow_mut() = Some(mode));
            assert!(reg.add_key(&mut meta, &mut keys, "password123", &fixtures[0], "crash").is_err());
            // No stack unwinding: exercise lock release and recovery after process termination.
            std::process::exit(73);
        }
    }

    fn child(reg: &Registry, mode: &str) -> std::process::Child {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "registry::tests::mutation_subprocess_worker", "--nocapture"])
            .env("QW_MUTATION_CHILD_ROOT", reg.paths.root())
            .env("QW_MUTATION_CHILD_MODE", mode)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap()
    }

    #[test]
    fn two_processes_preserve_imports_and_allocate_distinct_accounts() {
        let (reg, dir) = registry();
        let meta = reg.create_hd("main", PHRASE, "english", "", "password123", true).unwrap();
        let mut a = child(&reg, "0");
        let mut b = child(&reg, "1");
        wait_for_file(&dir.join("ready-0"));
        wait_for_file(&dir.join("ready-1"));
        std::fs::write(dir.join("go"), b"go").unwrap();
        assert!(a.wait().unwrap().success());
        assert!(b.wait().unwrap().success());
        let (current, keys) = reg.unlock_current(&meta.id, "password123").unwrap();
        assert_eq!(current.qi_imported.len(), 2);
        assert_eq!(keys.secrets().imported.len(), 2);
        assert_eq!(current.quai_accounts.len(), 3);
        assert_ne!(current.quai_accounts[1].hd_index, current.quai_accounts[2].hd_index);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn password_rotation_races_an_import_without_losing_either_key() {
        let (reg, dir) = registry();
        let mut meta = reg.create_hd("main", PHRASE, "english", "", "password123", true).unwrap();
        let mut keys = reg.unlock(&meta, "password123").unwrap();
        reg.add_key(&mut meta, &mut keys, "password123", &imported_fixture_keys()[0], "prior").unwrap();
        let mut import = child(&reg, "1");
        let mut rotation = child(&reg, "rotate");
        wait_for_file(&dir.join("ready-1"));
        wait_for_file(&dir.join("ready-rotate"));
        std::fs::write(dir.join("go"), b"go").unwrap();
        assert!(import.wait().unwrap().success());
        assert!(rotation.wait().unwrap().success());
        assert!(reg.unlock(&meta, "password123").is_err());
        let (current, keys) = reg.unlock_current(&meta.id, "rotatedpassword").unwrap();
        assert_eq!(current.qi_imported.len(), 2);
        assert_eq!(keys.secrets().imported.len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn process_exit_at_each_commit_boundary_recovers_on_reopen() {
        for boundary in ["journal", "vault", "metadata"] {
            let (reg, dir) = registry();
            let meta = reg.create_hd("main", PHRASE, "english", "", "password123", true).unwrap();
            assert_eq!(child(&reg, boundary).wait().unwrap().code(), Some(73));
            let (current, keys) = reg.unlock_current(&meta.id, "password123").unwrap();
            assert_eq!(current.generation, meta.generation + 1);
            assert_eq!(keys.secrets().imported.len(), 1);
            assert!(!reg.journal_path(&meta.id).exists());
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
