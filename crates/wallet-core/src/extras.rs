//! Prices, encrypted application backups and public status.

use crate::amount;
use crate::appdb::{AppDb, OpStatus};
use crate::error::{CoreError, Result};
use crate::paths::Paths;
use crate::registry::{Registry, WalletMeta, now};
use crate::session::Session;
use base64::Engine;
use quai_sdk::U256;
use quai_sdk::rpc::fetch::{FetchCancellation, FetchClient, FetchConfig, FetchRequest, NativeFetch};
use serde::{Deserialize, Serialize};
use std::path::Path;
use wallet_vault::VaultFile;

// ---------------------------------------------------------------- prices

/// Public QUAI/USD price source used by Pelagus.
pub const PRICE_URL: &str = "https://api.qu.ai/price";

/// A fetched price with its time.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Price {
    /// USD per QUAI.
    pub usd_per_quai: f64,
    /// Fetch time (unix seconds).
    pub fetched_at: u64,
    /// Source URL.
    pub source: String,
}

/// Fetch the QUAI/USD price. Display only; never used in amount arithmetic.
pub async fn fetch_price() -> Result<Price> {
    let backend = NativeFetch::new(5_000).map_err(|e| CoreError::Network(format!("price fetch: {e}")))?;
    let client = FetchClient::new(backend, FetchConfig::default().with_timeout_ms(8_000))
        .map_err(|e| CoreError::Network(format!("price fetch: {e}")))?;
    let request = FetchRequest::new(PRICE_URL).map_err(|e| CoreError::Network(format!("price fetch: {e}")))?;
    let fetched =
        client.send(&request, &FetchCancellation::default()).await.map_err(|e| CoreError::Network(format!("price fetch: {e}")))?;
    if !fetched.response.ok() {
        return Err(CoreError::Network(format!("price API returned HTTP {}", fetched.response.status())));
    }
    let value = fetched.response.json().map_err(|e| CoreError::Network(format!("price API response: {e}")))?;
    let price = value["price"]
        .as_f64()
        .filter(|p| p.is_finite() && *p > 0.0)
        .ok_or_else(|| CoreError::Network("price API returned no price".into()))?;
    Ok(Price { usd_per_quai: price, fetched_at: now(), source: PRICE_URL.into() })
}

/// Cached price (refreshed at most every 10 minutes).
pub async fn cached_price(app: &AppDb, fetch: bool) -> Option<Price> {
    let cached: Option<Price> = app.kv("price:quai_usd").ok().flatten().and_then(|t| serde_json::from_str(&t).ok());
    if let Some(p) = &cached
        && now().saturating_sub(p.fetched_at) < 600
    {
        return cached;
    }
    if !fetch {
        return cached;
    }
    match fetch_price().await {
        Ok(p) => {
            if let Ok(text) = serde_json::to_string(&p) {
                let _ = app.set_kv("price:quai_usd", &text);
            }
            Some(p)
        }
        Err(_) => cached,
    }
}

/// Approximate USD value of QUAI base units for display (2 decimals).
pub fn usd_value(its: U256, price: &Price) -> String {
    let whole = amount::format_amount_short(its, 18, 6);
    let parsed: f64 = whole.trim_start_matches('<').parse().unwrap_or(0.0);
    format!("${:.2}", parsed * price.usd_per_quai)
}

// ---------------------------------------------------------------- backups

const BACKUP_FORMAT: &str = "quai-wallet-backup";
const BACKUP_LIMIT: usize = 256 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct Archive {
    format: String,
    version: u32,
    created: u64,
    app_version: String,
    wallet: WalletMeta,
    vault: Option<String>,
    app_db: String,
    networks: Vec<NetworkState>,
    custom_networks: Vec<crate::network::NetworkProfile>,
}

#[derive(Serialize, Deserialize)]
struct NetworkState {
    id: String,
    quai: Option<String>,
    qi: Option<String>,
}

/// Summary of a verified backup.
#[derive(Clone, Debug, Serialize)]
pub struct BackupInfo {
    /// Wallet name.
    pub wallet: String,
    /// Wallet id.
    pub wallet_id: String,
    /// Creation time.
    pub created: u64,
    /// Networks with state.
    pub networks: Vec<String>,
    /// Quai accounts.
    pub accounts: usize,
    /// Contains the encrypted key vault.
    pub has_keys: bool,
}

fn sqlite_snapshot(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let src = rusqlite::Connection::open(path)?;
    let tmp = path.with_extension(format!("backup-{}.tmp", now()));
    {
        let mut dst = rusqlite::Connection::open(&tmp)?;
        let backup = rusqlite::backup::Backup::new(&src, &mut dst)?;
        backup.run_to_completion(128, std::time::Duration::from_millis(5), None)?;
    }
    let bytes = std::fs::read(&tmp)?;
    let _ = std::fs::remove_file(&tmp);
    Ok(Some(base64::engine::general_purpose::STANDARD.encode(bytes)))
}

/// Write an encrypted backup of one wallet (keys vault as stored, SDK custody state, app data).
pub fn create_backup(
    registry: &Registry,
    config: &crate::config::AppConfig,
    meta: &WalletMeta,
    out: &Path,
    password: &str,
) -> Result<BackupInfo> {
    let paths = registry.paths();
    let (meta, vault) = registry.encrypted_snapshot(&meta.id)?;
    let wallet_dir = paths.wallet_dir(&meta.id);
    let app_db = sqlite_snapshot(&wallet_dir.join("app.sqlite"))?.unwrap_or_default();
    let mut networks = Vec::new();
    if let Ok(entries) = std::fs::read_dir(wallet_dir.join("networks")) {
        for entry in entries.flatten() {
            let id = entry.file_name().to_string_lossy().to_string();
            networks.push(NetworkState {
                quai: sqlite_snapshot(&entry.path().join("quai.sqlite"))?,
                qi: sqlite_snapshot(&entry.path().join("qi.sqlite"))?,
                id,
            });
        }
    }
    let archive = Archive {
        format: BACKUP_FORMAT.into(),
        version: 1,
        created: now(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        wallet: meta.clone(),
        vault,
        app_db,
        networks,
        custom_networks: config.networks.clone(),
    };
    let bytes = zeroize::Zeroizing::new(serde_json::to_vec(&archive)?);
    let sealed = VaultFile::seal_bytes(&bytes, password, registry.kdf_params(), BACKUP_LIMIT)?;
    let sealed = VaultFile::from_json_limited(&sealed.to_json()?, BACKUP_LIMIT)?;
    sealed.open_bytes(password, registry.insecure_kdf(), BACKUP_LIMIT)?;
    wallet_vault::write_private_atomic(out, sealed.to_json()?.as_bytes())?;
    Ok(info(&archive))
}

fn info(a: &Archive) -> BackupInfo {
    BackupInfo {
        wallet: a.wallet.name.clone(),
        wallet_id: a.wallet.id.clone(),
        created: a.created,
        networks: a.networks.iter().map(|n| n.id.clone()).collect(),
        accounts: a.wallet.quai_accounts.len(),
        has_keys: a.vault.is_some(),
    }
}

fn read_archive(registry: &Registry, path: &Path, password: &str) -> Result<Archive> {
    let text = std::fs::read_to_string(path)?;
    let file = VaultFile::from_json_limited(&text, BACKUP_LIMIT)?;
    let bytes = file.open_bytes(password, registry.insecure_kdf(), BACKUP_LIMIT).map_err(|e| match e {
        wallet_vault::VaultError::Authentication => CoreError::Locked("incorrect backup password or corrupted backup".into()),
        other => other.into(),
    })?;
    let archive: Archive = serde_json::from_slice(&bytes).map_err(|_| CoreError::Invalid("backup content is malformed".into()))?;
    if archive.format != BACKUP_FORMAT || archive.version != 1 {
        return Err(CoreError::Invalid("unsupported backup format".into()));
    }
    crate::paths::validate_id(&archive.wallet.id)?;
    for n in &archive.networks {
        crate::paths::validate_id(&n.id)?;
    }
    Ok(archive)
}

/// Decrypt and validate a backup without restoring it.
pub fn verify_backup(registry: &Registry, path: &Path, password: &str) -> Result<BackupInfo> {
    Ok(info(&read_archive(registry, path, password)?))
}

/// Restore a wallet from an encrypted backup.
pub fn restore_backup(
    registry: &Registry,
    config: &mut crate::config::AppConfig,
    paths: &Paths,
    path: &Path,
    password: &str,
) -> Result<BackupInfo> {
    let archive = read_archive(registry, path, password)?;
    let b64 = base64::engine::general_purpose::STANDARD;
    registry.install_restored(&archive.wallet, archive.vault.as_deref())?;
    let wallet_dir = paths.wallet_dir(&archive.wallet.id);
    let write_db = |dest: &Path, data: &str| -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let bytes = b64.decode(data).map_err(|_| CoreError::Invalid("backup database encoding".into()))?;
        wallet_vault::write_private_atomic(dest, &bytes)?;
        Ok(())
    };
    let result = (|| -> Result<()> {
        write_db(&wallet_dir.join("app.sqlite"), &archive.app_db)?;
        for n in &archive.networks {
            let dir = wallet_dir.join("networks").join(&n.id);
            crate::paths::ensure_private_dir(&dir)?;
            if let Some(q) = &n.quai {
                write_db(&dir.join("quai.sqlite"), q)?;
            }
            if let Some(q) = &n.qi {
                write_db(&dir.join("qi.sqlite"), q)?;
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&wallet_dir);
        return Err(e);
    }
    let mut changed = false;
    for n in archive.custom_networks.iter() {
        if !config.networks.iter().any(|c| c.id == n.id) && !crate::network::NetworkProfile::builtins().iter().any(|b| b.id == n.id) {
            config.networks.push(n.clone());
            changed = true;
        }
    }
    if changed {
        config.save(paths)?;
    }
    Ok(info(&archive))
}

// ---------------------------------------------------------------- status

/// Public status for status bars (no balances unless requested).
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct PublicStatus {
    /// Wallet name.
    pub wallet: String,
    /// Network id.
    pub network: String,
    /// Latest block height seen.
    pub height: Option<u64>,
    /// Node reachable and matching.
    pub node_ok: bool,
    /// Unlocked in a running daemon.
    pub unlocked: bool,
    /// Operations needing attention (unknown/pending/settling).
    pub attention: usize,
    /// Unread notifications.
    pub unread: usize,
    /// Status time.
    pub updated: u64,
    /// Optional balances (only when enabled).
    pub balances: Option<serde_json::Value>,
}

impl PublicStatus {
    /// Waybar JSON: `{text, tooltip, class}`.
    pub fn waybar(&self) -> serde_json::Value {
        // Nerd Font glyphs (Omarchy's Waybar font): fa-unlock / fa-lock.
        let lock = if self.unlocked { "\u{f09c}" } else { "\u{f023}" };
        let mut text = format!("{lock} {}", if self.node_ok { "●" } else { "○" });
        if self.attention > 0 {
            text.push_str(&format!(" !{}", self.attention));
        }
        let class = if !self.node_ok {
            "offline"
        } else if self.attention > 0 {
            "attention"
        } else {
            "ok"
        };
        let mut tooltip = format!(
            "Quai Terminal · {} on {}\nnode {} · block {}\n{} · {} attention · {} unread",
            self.wallet,
            self.network,
            if self.node_ok { "ok" } else { "unreachable" },
            self.height.map_or("?".into(), |h| h.to_string()),
            if self.unlocked { "unlocked" } else { "locked" },
            self.attention,
            self.unread
        );
        if let Some(b) = &self.balances {
            tooltip.push_str(&format!("\n{b}"));
        }
        serde_json::json!({"text": text, "tooltip": tooltip, "class": class, "alt": class})
    }
}

/// Read the last status written by a daemon.
pub fn read_status(paths: &Paths) -> Option<PublicStatus> {
    std::fs::read_to_string(paths.status_file()).ok().and_then(|t| serde_json::from_str(&t).ok())
}

/// Write the status file atomically.
pub fn write_status(paths: &Paths, status: &PublicStatus) -> Result<()> {
    wallet_vault::write_private_atomic(&paths.status_file(), serde_json::to_string(status)?.as_bytes())?;
    Ok(())
}

impl Session {
    /// Compute public status (one light node request).
    pub async fn public_status(&mut self, include_balances: bool) -> PublicStatus {
        let height = self.head().await.ok();
        let attention = self
            .app
            .open_operations(&self.network.id)
            .map(|ops| ops.iter().filter(|o| o.status != OpStatus::Prepared).count())
            .unwrap_or(0);
        let unread = self.app.notifications(100).map(|n| n.iter().filter(|x| !x.read).count()).unwrap_or(0);
        let balances = if include_balances {
            let quai: U256 =
                self.quai_balances().await.map(|b| b.iter().fold(U256::ZERO, |acc, x| acc.saturating_add(x.balance))).unwrap_or_default();
            let qi = self.qi_summary().map(|s| s.balance.total).unwrap_or_default();
            Some(serde_json::json!({"quai": amount::quai(quai), "qi": amount::qi(qi)}))
        } else {
            None
        };
        PublicStatus {
            wallet: self.meta.name.clone(),
            network: self.network.id.clone(),
            node_ok: height.is_some(),
            height,
            unlocked: self.is_unlocked(),
            attention,
            unread,
            updated: now(),
            balances,
        }
    }
}
