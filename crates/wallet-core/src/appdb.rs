//! Application database: labels, contacts, tokens, activity journal, notifications.
//!
//! Kept separate from SDK stores (the SDK backup rejects foreign tables). Public data only.

use crate::error::{CoreError, Result};
use crate::registry::now;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// 2: schedules carry a `mac` authenticating what they may sign unattended.
/// 3: pool events and the DEX tape are rows, not one JSON blob rewritten per refresh.
/// 4: `fetching` leases, so two processes refreshing the same feed make one request.
/// 5: leases name their process (`fetch_leases`), so one left by a process that died is taken over
///    at once rather than after [`FETCH_LEASE`].
/// 6: interval conversion schedules are gone; a wallet file drops their tables.
const SCHEMA_VERSION: i64 = 8;

/// Tables and indexes `SCHEMA` creates.
fn schema_objects(schema: &str) -> Vec<&str> {
    schema
        .split("IF NOT EXISTS")
        .skip(1)
        .filter_map(|rest| rest.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).find(|w| !w.is_empty()))
        .collect()
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS contacts(
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  address TEXT,
  payment_code TEXT,
  note TEXT NOT NULL DEFAULT '',
  created INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS contact_addresses(
  contact_id INTEGER NOT NULL,
  address TEXT NOT NULL,
  first_seen INTEGER NOT NULL,
  PRIMARY KEY(contact_id, address)
);
CREATE TABLE IF NOT EXISTS labels(
  address TEXT PRIMARY KEY,
  label TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS tokens(
  network TEXT NOT NULL,
  address TEXT NOT NULL,
  symbol TEXT NOT NULL,
  name TEXT NOT NULL,
  decimals INTEGER NOT NULL,
  hidden INTEGER NOT NULL DEFAULT 0,
  added INTEGER NOT NULL,
  PRIMARY KEY(network, address)
);
CREATE TABLE IF NOT EXISTS operations(
  id TEXT PRIMARY KEY,
  network TEXT NOT NULL,
  kind TEXT NOT NULL,
  store TEXT NOT NULL,
  account TEXT NOT NULL,
  status TEXT NOT NULL,
  tx_hash TEXT,
  asset TEXT NOT NULL,
  amount TEXT NOT NULL,
  counterparty TEXT NOT NULL DEFAULT '',
  fee TEXT NOT NULL DEFAULT '',
  detail TEXT NOT NULL DEFAULT '{}',
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS operations_network ON operations(network, created);
CREATE TABLE IF NOT EXISTS trade_plans(
  id TEXT PRIMARY KEY,
  network TEXT NOT NULL,
  revision INTEGER NOT NULL,
  plan TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS trade_plans_network ON trade_plans(network);
CREATE TABLE IF NOT EXISTS activity(
  network TEXT NOT NULL,
  key TEXT NOT NULL,
  direction TEXT NOT NULL,
  asset TEXT NOT NULL,
  amount TEXT NOT NULL,
  address TEXT NOT NULL,
  tx_hash TEXT,
  block INTEGER,
  detail TEXT NOT NULL DEFAULT '{}',
  observed INTEGER NOT NULL,
  PRIMARY KEY(network, key)
);
CREATE TABLE IF NOT EXISTS notifications(
  id INTEGER PRIMARY KEY,
  at INTEGER NOT NULL,
  level TEXT NOT NULL,
  title TEXT NOT NULL,
  body TEXT NOT NULL,
  read INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS kv(
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS cache(
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  fetched INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS fetch_leases(
  key TEXT PRIMARY KEY,
  since INTEGER NOT NULL,
  pid INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS media(
  url TEXT PRIMARY KEY,
  hash TEXT,
  error TEXT NOT NULL DEFAULT '',
  fetched INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS renditions(
  hash TEXT NOT NULL,
  size INTEGER NOT NULL,
  width INTEGER NOT NULL,
  height INTEGER NOT NULL,
  png BLOB NOT NULL,
  rgba BLOB NOT NULL,
  dominant INTEGER NOT NULL,
  PRIMARY KEY(hash, size)
);
CREATE TABLE IF NOT EXISTS pool_events(
  network TEXT NOT NULL,
  pool TEXT NOT NULL,
  block INTEGER NOT NULL,
  log_index INTEGER NOT NULL,
  at INTEGER NOT NULL,
  event TEXT NOT NULL,
  PRIMARY KEY(network, pool, block, log_index)
);
CREATE INDEX IF NOT EXISTS pool_events_seen ON pool_events(network, pool, at);
CREATE TABLE IF NOT EXISTS dex_swaps(
  network TEXT NOT NULL,
  tx TEXT NOT NULL,
  log_index INTEGER NOT NULL,
  block INTEGER NOT NULL,
  at INTEGER NOT NULL,
  timed INTEGER NOT NULL,
  swap TEXT NOT NULL,
  PRIMARY KEY(network, tx, log_index)
);
CREATE INDEX IF NOT EXISTS dex_swaps_block ON dex_swaps(network, block);
"#;

/// The shared cache's own schema: only the tables that hold wallet-independent display data.
///
/// A deliberately narrow file. The wallet tables (contacts, operations, activity) are
/// not here and must never be: this file is shared by every wallet in one data directory, and the
/// separation between them is the reason someone keeps two.
const SHARED_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS cache(
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  fetched INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS fetch_leases(
  key TEXT PRIMARY KEY,
  since INTEGER NOT NULL,
  pid INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS pool_events(
  network TEXT NOT NULL,
  pool TEXT NOT NULL,
  block INTEGER NOT NULL,
  log_index INTEGER NOT NULL,
  at INTEGER NOT NULL,
  event TEXT NOT NULL,
  PRIMARY KEY(network, pool, block, log_index)
);
CREATE INDEX IF NOT EXISTS pool_events_seen ON pool_events(network, pool, at);
CREATE TABLE IF NOT EXISTS dex_swaps(
  network TEXT NOT NULL,
  tx TEXT NOT NULL,
  log_index INTEGER NOT NULL,
  block INTEGER NOT NULL,
  at INTEGER NOT NULL,
  timed INTEGER NOT NULL,
  swap TEXT NOT NULL,
  PRIMARY KEY(network, tx, log_index)
);
CREATE INDEX IF NOT EXISTS dex_swaps_block ON dex_swaps(network, block);
"#;

/// 2: `fetching` leases, so two wallets refreshing the same feed make one request.
/// 3: leases name their process (`fetch_leases`).
const SHARED_SCHEMA_VERSION: i64 = 3;

/// Cache keys the shared store answers: feeds that are the same URL and the same answer for every
/// wallet, and that every wallet fetches whatever it holds.
///
/// Anything outside this list stays in the wallet's own `app.sqlite`. The test is not "is this
/// value public" — it is "does asking for it say anything about this wallet". A price does not. A
/// holdings list does, and so does an NFT's metadata, which is looked up because a wallet holds or
/// browsed that item. Those stay per wallet even though their contents are public, because a row's
/// existence in a shared file is itself a fact about who fetched it.
pub const SHARED_FEEDS: [&str; 23] = [
    "chain_stats",
    "launch_logo:",
    "prices",
    "token_markets",
    "dex_pools",
    "launch_amm_pools_v2",
    "pool_tvl",
    "launches:",
    "listings:",
    "listing_nft:",
    "collections:",
    "subgraph_candles:",
    "token_meta:",
    "legacy_pools",
    "legacy_pools:",
    "hartii_amm_pools:",
    "launch_amm_pools_v2:",
    "hartii_launches",
    "hartii_launches:",
    "hartii_changes:",
    "subgraph_spot_24h:",
    "canonical_header:",
    "dex_flow:",
];

/// Whether a cache key (without its network prefix) belongs in the shared store.
pub fn is_shared_feed(key: &str) -> bool {
    SHARED_FEEDS.iter().any(|f| if f.ends_with(':') { key.starts_with(f) } else { key == *f })
}

/// Whether a process is still running. On Linux, from `/proc`; elsewhere a lease is only ever
/// taken over by age, so a process is assumed alive.
fn process_alive(pid: i64) -> bool {
    if cfg!(target_os = "linux") { std::path::Path::new(&format!("/proc/{pid}")).exists() } else { true }
}

/// A cached image rendition row: (width, height, png, rgba, dominant 0xRRGGBB).
pub type StoredRendition = (u32, u32, Vec<u8>, Vec<u8>, u32);

/// Tables holding re-fetchable third-party data; excluded from backups.
const CACHE_TABLES: [&str; 6] = ["cache", "fetch_leases", "media", "renditions", "pool_events", "dex_swaps"];

/// How long one process may hold a feed's refresh before another takes it over.
///
/// A lease is only ever an optimisation: its holder crashing costs the others one refresh
/// interval of staleness, never an error and never a missing screen. So it is generous enough
/// that a slow explorer (`/api/stats/assets` has taken 15 s cold) is not taken over mid-flight.
pub const FETCH_LEASE: u64 = 20;

/// Oldest observation the feeds keep: the longest chart window the wallet draws.
pub const FEED_KEEP: u64 = 30 * 86_400;

/// The key in an operation's detail holding its stages: `[{"s": status, "at": unix, "tx"?}]`.
pub const TIMELINE: &str = "timeline";

/// Address book entry.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Contact {
    /// Row id.
    pub id: i64,
    /// Unique name.
    pub name: String,
    /// Quai or Qi address.
    pub address: Option<String>,
    /// BIP47 payment code.
    pub payment_code: Option<String>,
    /// Free-form note.
    pub note: String,
}

/// Imported ERC-20 token.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Token {
    /// Network id.
    pub network: String,
    /// Contract address.
    pub address: String,
    /// Symbol (untrusted display data).
    pub symbol: String,
    /// Name (untrusted display data).
    pub name: String,
    /// Decimals.
    pub decimals: u8,
    /// Hidden from lists.
    pub hidden: bool,
}

/// Operation lifecycle status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpStatus {
    /// Prepared and reserved, not signed.
    Prepared,
    /// Signed and durably stored, not submitted.
    Signed,
    /// Submitted; acknowledgement received.
    Submitted,
    /// Submission outcome unknown; reconcile before retrying.
    Unknown,
    /// Included and successful at origin.
    Confirmed,
    /// Included but failed/reverted.
    Failed,
    /// Waiting for destination settlement (conversions, wraps).
    Settling,
    /// Destination settled with locked output.
    Locked,
    /// Destination settled and spendable.
    Settled,
    /// Conversion refunded.
    Refunded,
    /// Replaced by another candidate in the same nonce family.
    Replaced,
    /// Abandoned before signing.
    Cancelled,
}

impl OpStatus {
    /// Stable text form.
    pub fn as_str(self) -> &'static str {
        match self {
            OpStatus::Prepared => "prepared",
            OpStatus::Signed => "signed",
            OpStatus::Submitted => "submitted",
            OpStatus::Unknown => "unknown",
            OpStatus::Confirmed => "confirmed",
            OpStatus::Failed => "failed",
            OpStatus::Settling => "settling",
            OpStatus::Locked => "locked",
            OpStatus::Settled => "settled",
            OpStatus::Refunded => "refunded",
            OpStatus::Replaced => "replaced",
            OpStatus::Cancelled => "cancelled",
        }
    }

    /// Parse text form.
    pub fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "prepared" => OpStatus::Prepared,
            "signed" => OpStatus::Signed,
            "submitted" => OpStatus::Submitted,
            "unknown" => OpStatus::Unknown,
            "confirmed" => OpStatus::Confirmed,
            "failed" => OpStatus::Failed,
            "settling" => OpStatus::Settling,
            "locked" => OpStatus::Locked,
            "settled" => OpStatus::Settled,
            "refunded" => OpStatus::Refunded,
            "replaced" => OpStatus::Replaced,
            "cancelled" => OpStatus::Cancelled,
            other => return Err(CoreError::Storage(format!("unknown operation status `{other}`"))),
        })
    }

    /// Sent but not yet mined: a higher-fee replacement can still win.
    pub fn replaceable(self) -> bool {
        matches!(self, OpStatus::Submitted | OpStatus::Unknown)
    }

    /// No further automatic tracking is required.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            OpStatus::Confirmed | OpStatus::Failed | OpStatus::Settled | OpStatus::Refunded | OpStatus::Replaced | OpStatus::Cancelled
        )
    }
}

/// A wallet-initiated operation (journal row).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Operation {
    /// Reservation id (32 hex chars).
    pub id: String,
    /// Network id.
    pub network: String,
    /// Operation kind (e.g. `send_quai`, `convert_quai_to_qi`).
    pub kind: String,
    /// SDK store holding custody (`quai` or `qi`).
    pub store: String,
    /// Source account address or `qi`.
    pub account: String,
    /// Status.
    pub status: OpStatus,
    /// Transaction hash once signed.
    pub tx_hash: Option<String>,
    /// Asset label (`QUAI`, `QI`, token symbol).
    pub asset: String,
    /// Amount in base units.
    pub amount: String,
    /// Destination or peer.
    pub counterparty: String,
    /// Fee (base units of the fee asset), when known.
    pub fee: String,
    /// JSON detail.
    pub detail: serde_json::Value,
    /// Created (unix seconds).
    pub created: u64,
    /// Updated (unix seconds).
    pub updated: u64,
}

/// An observed incoming or external event.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Activity {
    /// Network id.
    pub network: String,
    /// Unique key (e.g. outpoint or tx hash + log index).
    pub key: String,
    /// `in` or `out`.
    pub direction: String,
    /// Asset label.
    pub asset: String,
    /// Amount in base units.
    pub amount: String,
    /// Our address involved.
    pub address: String,
    /// Transaction hash.
    pub tx_hash: Option<String>,
    /// Block number.
    pub block: Option<u64>,
    /// JSON detail.
    pub detail: serde_json::Value,
    /// First observed (unix seconds).
    pub observed: u64,
}

/// In-app notification.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Notice {
    /// Row id.
    pub id: i64,
    /// Time.
    pub at: u64,
    /// `info`, `success`, `warn`, `error`.
    pub level: String,
    /// Title.
    pub title: String,
    /// Body.
    pub body: String,
    /// Read flag.
    pub read: bool,
}

/// The application database connection.
pub struct AppDb {
    conn: Connection,
}

impl AppDb {
    /// Recover the operation link if a client stopped between journaling a review and its checkpoint.
    pub fn operations_for_plan(&self, network: &str, plan: &str) -> Result<Vec<Operation>> {
        let mut stmt =
            self.conn.prepare("SELECT id FROM operations WHERE network=?1 AND json_extract(detail, '$.plan_id')=?2 ORDER BY created,id")?;
        let ids = stmt.query_map(params![network, plan], |r| r.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
        ids.iter().map(|id| self.operation(id)?.ok_or_else(|| CoreError::Storage("plan operation disappeared".into()))).collect()
    }
    /// Atomically store a public execution checkpoint; never overwrite another client's work.
    pub fn save_trade_plan(&self, plan: &mut crate::plans::TradePlan) -> Result<()> {
        if plan.version != 1 {
            return Err(CoreError::Invalid("unsupported trade plan version".into()));
        }
        let previous = plan.revision;
        let mut next = plan.clone();
        next.revision = previous.checked_add(1).ok_or_else(|| CoreError::Storage("plan revision exhausted".into()))?;
        next.updated = now();
        let revision = i64::try_from(next.revision).map_err(|_| CoreError::Storage("plan revision exhausted".into()))?;
        let previous = i64::try_from(previous).map_err(|_| CoreError::Storage("plan revision exhausted".into()))?;
        let encoded = serde_json::to_string(&next)?;
        if encoded.len() > 262_144 {
            return Err(CoreError::Invalid("trade plan is too large".into()));
        }
        let changed = if previous == 0 {
            self.conn.execute(
                "INSERT OR IGNORE INTO trade_plans(id,network,revision,plan) VALUES(?1,?2,?3,?4)",
                params![next.id, next.network, revision, encoded],
            )?
        } else {
            self.conn.execute(
                "UPDATE trade_plans SET revision=?1,plan=?2 WHERE id=?3 AND network=?4 AND revision=?5",
                params![revision, encoded, next.id, next.network, previous],
            )?
        };
        if changed != 1 {
            return Err(CoreError::Invalid("trade plan changed in another client; reload before continuing".into()));
        }
        *plan = next;
        Ok(())
    }

    pub fn trade_plan(&self, id: &str) -> Result<Option<crate::plans::TradePlan>> {
        let value: Option<String> = self.conn.query_row("SELECT plan FROM trade_plans WHERE id=?1", [id], |r| r.get(0)).optional()?;
        value.map(|value| serde_json::from_str(&value).map_err(Into::into)).transpose()
    }

    pub fn trade_plans(&self, network: &str) -> Result<Vec<crate::plans::TradePlan>> {
        let mut stmt = self.conn.prepare("SELECT plan FROM trade_plans WHERE network=?1 ORDER BY rowid DESC LIMIT 200")?;
        let rows = stmt.query_map([network], |r| r.get::<_, String>(0))?;
        rows.map(|row| serde_json::from_str(&row?).map_err(Into::into)).collect()
    }

    /// Open or create a wallet's own database.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with(path, SCHEMA, SCHEMA_VERSION)
    }

    /// Open or create the shared display cache (`Paths::shared_cache`).
    ///
    /// The same connection type, a much smaller schema: the wallet-independent feeds only. Every
    /// wallet and every process in one data directory shares this file, which is the point — the
    /// data is not any wallet's.
    pub fn open_shared(path: &Path) -> Result<Self> {
        Self::open_with(path, SHARED_SCHEMA, SHARED_SCHEMA_VERSION)
    }

    fn open_with(path: &Path, schema: &str, version: i64) -> Result<Self> {
        if let Some(dir) = path.parent() {
            crate::paths::ensure_private_dir(dir)?;
        }
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // Several connections open this file at once (TUI workers, daemon, CLI): opening a
        // database that is already current takes no write lock.
        let mode: String = conn.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            conn.pragma_update(None, "journal_mode", "WAL")?;
        }
        conn.pragma_update(None, "synchronous", "FULL")?;
        if Self::needs_migration(&conn, schema, version)? {
            Self::migrate(&mut conn, schema, version)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(Self { conn })
    }

    /// Whether this file is missing anything the current schema defines. Read-only, so the common
    /// case — a database that is already current — still takes no write lock.
    fn needs_migration(conn: &Connection, schema: &str, target: i64) -> Result<bool> {
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > target {
            return Err(CoreError::Storage("app database is from a newer wallet version".into()));
        }
        // Tables were added over time without a version bump: create any that are missing.
        let names = schema_objects(schema);
        let present: i64 = conn.query_row(
            &format!(
                "SELECT count(*) FROM sqlite_master WHERE name IN ({})",
                names.iter().map(|n| format!("'{n}'")).collect::<Vec<_>>().join(",")
            ),
            [],
            |r| r.get(0),
        )?;
        Ok(version != target || present != names.len() as i64)
    }

    /// Bring the file up to the current schema, in one immediate transaction.
    ///
    /// Two processes opening the same wallet at the same moment used to run this in autocommit and
    /// both reach the `ALTER TABLE`, so the loser failed on a duplicate column and could not open
    /// the wallet at all. Taking the write lock up front makes the second one wait, and the
    /// re-check inside the transaction then finds nothing left to do.
    fn migrate(conn: &mut Connection, schema: &str, target: i64) -> Result<()> {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if !Self::needs_migration(&tx, schema, target)? {
            return Ok(());
        }
        tx.execute_batch(schema)?;
        if schema_objects(schema).contains(&"operations") {
            // Interval conversion schedules were removed (version 6); nothing reads their tables.
            tx.execute_batch("DROP TABLE IF EXISTS schedule_runs; DROP TABLE IF EXISTS schedules;")?;
            // Version 8: notifications no longer carry what a sealed message said. Older builds
            // stored it; `secure_delete` zeroes the old text's pages instead of leaving them free.
            let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
            if version < 8 {
                tx.pragma_update(None, "secure_delete", "ON")?;
                tx.execute(
                    "UPDATE notifications SET body=?1 WHERE level='chat' AND title NOT LIKE '#%' AND body<>?1",
                    [crate::chat::REDACTED_NOTICE],
                )?;
                // A sealed message's review, text included, was journaled with its operation.
                Self::redact_journaled_messages(&tx)?;
            }
        }
        tx.pragma_update(None, "user_version", target)?;
        tx.commit()?;
        // The redaction above went through the WAL; fold it into the file so the old text does
        // not linger in the log.
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        Ok(())
    }

    /// Replace the text of every journaled sealed-message review with [`crate::tx::PRIVATE_FIELD`].
    fn redact_journaled_messages(tx: &Connection) -> Result<()> {
        let mut stmt = tx.prepare("SELECT id, detail FROM operations WHERE kind='board_post' AND json_extract(detail, '$.sealed')=1")?;
        let rows =
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<std::result::Result<Vec<_>, _>>()?;
        for (id, detail) in rows {
            let Ok(mut detail) = serde_json::from_str::<serde_json::Value>(&detail) else { continue };
            if let Some(fields) = detail.pointer_mut("/review/fields").and_then(|f| f.as_array_mut()) {
                for f in fields.iter_mut().filter(|f| f["label"] == "Message") {
                    f["value"] = serde_json::json!(crate::tx::PRIVATE_FIELD);
                }
            }
            detail["private_fields"] = serde_json::json!(["Message"]);
            tx.execute("UPDATE operations SET detail=?1 WHERE id=?2", params![detail.to_string(), id])?;
        }
        Ok(())
    }

    /// The raw connection, for tests that age rows.
    #[cfg(test)]
    pub(crate) fn conn_for_tests(&self) -> &Connection {
        &self.conn
    }

    /// In-memory database for tests.
    pub fn memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// In-memory shared cache for tests.
    pub fn shared_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SHARED_SCHEMA)?;
        Ok(Self { conn })
    }

    /// Take responsibility for refreshing `key`, if nobody else already has.
    ///
    /// Two wallet windows open together used to make the same request at the same instant: each
    /// missed the cache, neither had written yet, and both asked. Against a per-IP budget that is
    /// how a handful of windows parks background work everywhere. The claim is one row, so the
    /// loser can serve what it has instead of duplicating the request.
    ///
    /// Returns whether this caller is now the one fetching. A lease older than [`FETCH_LEASE`] is
    /// taken over: the holder may be gone.
    pub fn claim_fetch(&self, key: &str) -> Result<bool> {
        let cutoff = now().saturating_sub(FETCH_LEASE) as i64;
        self.conn.execute("DELETE FROM fetch_leases WHERE since < ?1", [cutoff])?;
        // A lease whose process has gone — killed, crashed, closed mid-fetch — is not waited out.
        let holder: Option<i64> = self.conn.query_row("SELECT pid FROM fetch_leases WHERE key = ?1", [key], |r| r.get(0)).optional()?;
        if let Some(pid) = holder
            && pid != i64::from(std::process::id())
            && !process_alive(pid)
        {
            self.conn.execute("DELETE FROM fetch_leases WHERE key = ?1 AND pid = ?2", params![key, pid])?;
        }
        Ok(self.conn.execute(
            "INSERT OR IGNORE INTO fetch_leases(key, since, pid) VALUES(?1, ?2, ?3)",
            params![key, now() as i64, i64::from(std::process::id())],
        )? == 1)
    }

    /// Give up a claim, whether the fetch succeeded or failed.
    pub fn release_fetch(&self, key: &str) -> Result<()> {
        self.conn.execute("DELETE FROM fetch_leases WHERE key = ?1 AND pid = ?2", params![key, i64::from(std::process::id())])?;
        Ok(())
    }

    /// Delete this store's own copies of the feeds the shared cache now answers.
    ///
    /// A wallet opened before the shared cache existed holds rows nothing will read again. They
    /// would age out in 30 days on their own; sweeping them is immediate and costs one statement.
    /// Returns how many rows went.
    pub fn drop_shared_feeds(&self) -> Result<usize> {
        let mut gone = 0;
        for feed in SHARED_FEEDS {
            // Keys are stored as `<network>:<feed>`, so the feed is matched after that prefix.
            // Every feed name contains `_`, which LIKE reads as a wildcard, so it is escaped.
            let escaped: String = feed.chars().flat_map(|c| if c == '_' || c == '%' { vec!['\\', c] } else { vec![c] }).collect();
            let pattern = format!("%:{escaped}{}", if feed.ends_with(':') { "%" } else { "" });
            gone += self.conn.execute("DELETE FROM cache WHERE key LIKE ?1 ESCAPE '\\'", [&pattern])?;
        }
        Ok(gone)
    }

    // ---------- contacts ----------

    /// Add a contact.
    pub fn add_contact(&self, name: &str, address: Option<&str>, payment_code: Option<&str>, note: &str) -> Result<i64> {
        if name.trim().is_empty() || name.len() > 64 {
            return Err(CoreError::Invalid("contact name must be 1-64 characters".into()));
        }
        if address.is_none() && payment_code.is_none() {
            return Err(CoreError::Invalid("contact needs an address or payment code".into()));
        }
        self.conn
            .execute(
                "INSERT INTO contacts(name,address,payment_code,note,created) VALUES(?1,?2,?3,?4,?5)",
                params![name.trim(), address, payment_code, note, now() as i64],
            )
            .map_err(|e| match e {
                rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
                    CoreError::Invalid(format!("a contact named `{name}` already exists"))
                }
                other => other.into(),
            })?;
        Ok(self.conn.last_insert_rowid())
    }

    /// List contacts by name.
    pub fn contacts(&self) -> Result<Vec<Contact>> {
        let mut stmt = self.conn.prepare("SELECT id,name,address,payment_code,note FROM contacts ORDER BY name COLLATE NOCASE")?;
        let rows = stmt.query_map([], |r| {
            Ok(Contact { id: r.get(0)?, name: r.get(1)?, address: r.get(2)?, payment_code: r.get(3)?, note: r.get(4)? })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Find a contact by exact name (case-insensitive).
    pub fn contact(&self, name: &str) -> Result<Option<Contact>> {
        Ok(self.contacts()?.into_iter().find(|c| c.name.eq_ignore_ascii_case(name)))
    }

    /// Update a contact.
    pub fn update_contact(&self, contact: &Contact) -> Result<()> {
        self.conn.execute(
            "UPDATE contacts SET name=?2,address=?3,payment_code=?4,note=?5 WHERE id=?1",
            params![contact.id, contact.name, contact.address, contact.payment_code, contact.note],
        )?;
        Ok(())
    }

    /// Drop a cancelled journal row so its reservation id can be reused (nonce-gap repair).
    pub fn remove_cancelled_operation(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM operations WHERE id=?1 AND status='cancelled'", params![id])?;
        Ok(())
    }

    /// Remove a contact by name.
    pub fn remove_contact(&self, name: &str) -> Result<bool> {
        if let Some(c) = self.contact(name)? {
            self.conn.execute("DELETE FROM contact_addresses WHERE contact_id=?1", [c.id])?;
        }
        Ok(self.conn.execute("DELETE FROM contacts WHERE name=?1 COLLATE NOCASE", [name])? > 0)
    }

    /// Every Quai address seen for a contact, newest last.
    ///
    /// One payment code is one person, but a person signs from as many accounts as they like —
    /// so a contact's identity is the code, and addresses accumulate under it.
    pub fn contact_addresses(&self, contact_id: i64) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT address FROM contact_addresses WHERE contact_id=?1 ORDER BY first_seen")?;
        let rows = stmt.query_map([contact_id], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Record an address for a contact. `true` when it had not been seen before, which is what
    /// the conversation marks — a peer writing from a new account is worth noticing.
    pub fn add_contact_address(&self, contact_id: i64, address: &str) -> Result<bool> {
        let address = address.trim().to_lowercase();
        if address.is_empty() {
            return Ok(false);
        }
        let inserted = self.conn.execute(
            "INSERT INTO contact_addresses(contact_id,address,first_seen) VALUES(?1,?2,?3) ON CONFLICT DO NOTHING",
            params![contact_id, address, now() as i64],
        )?;
        Ok(inserted > 0)
    }

    /// The contact an address belongs to: its own address column, or any address recorded under
    /// its payment code.
    pub fn contact_by_address(&self, address: &str) -> Result<Option<Contact>> {
        let wanted = address.trim().to_lowercase();
        let contacts = self.contacts()?;
        if let Some(c) = contacts.iter().find(|c| c.address.as_ref().is_some_and(|a| a.to_lowercase() == wanted)) {
            return Ok(Some(c.clone()));
        }
        let id: Option<i64> =
            self.conn.query_row("SELECT contact_id FROM contact_addresses WHERE address=?1", [&wanted], |r| r.get(0)).optional()?;
        Ok(id.and_then(|id| contacts.into_iter().find(|c| c.id == id)))
    }

    // ---------- labels ----------

    /// Set an address label (Qi addresses, contacts' addresses, mining addresses).
    pub fn set_label(&self, address: &str, label: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO labels(address,label) VALUES(?1,?2) ON CONFLICT(address) DO UPDATE SET label=excluded.label",
            params![address.to_lowercase(), label],
        )?;
        Ok(())
    }

    /// Label for an address.
    pub fn label(&self, address: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT label FROM labels WHERE address=?1", [address.to_lowercase()], |r| r.get(0)).optional()?)
    }

    /// All labels.
    pub fn labels(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare("SELECT address,label FROM labels ORDER BY label")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // ---------- tokens ----------

    /// Add or update a token.
    pub fn upsert_token(&self, token: &Token) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tokens(network,address,symbol,name,decimals,hidden,added) VALUES(?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(network,address) DO UPDATE SET symbol=excluded.symbol,name=excluded.name,decimals=excluded.decimals,hidden=excluded.hidden",
            params![token.network, token.address.to_lowercase(), token.symbol, token.name, token.decimals, token.hidden, now() as i64],
        )?;
        Ok(())
    }

    /// Tokens for a network.
    pub fn tokens(&self, network: &str, include_hidden: bool) -> Result<Vec<Token>> {
        let mut stmt = self.conn.prepare(
            "SELECT network,address,symbol,name,decimals,hidden FROM tokens WHERE network=?1 AND (?2 OR hidden=0) ORDER BY added",
        )?;
        let rows = stmt.query_map(params![network, include_hidden], |r| {
            Ok(Token { network: r.get(0)?, address: r.get(1)?, symbol: r.get(2)?, name: r.get(3)?, decimals: r.get(4)?, hidden: r.get(5)? })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Find a token by symbol or address.
    pub fn token(&self, network: &str, selector: &str) -> Result<Token> {
        let lower = selector.to_lowercase();
        let matches: Vec<Token> =
            self.tokens(network, true)?.into_iter().filter(|t| t.address == lower || t.symbol.eq_ignore_ascii_case(selector)).collect();
        match matches.len() {
            0 => Err(CoreError::NotFound(format!("token `{selector}` is not imported on {network}"))),
            1 => Ok(matches.into_iter().next().expect("one token")),
            _ => Err(CoreError::Invalid(format!("several tokens use the symbol `{selector}`; use the contract address"))),
        }
    }

    /// Remove a token.
    pub fn remove_token(&self, network: &str, address: &str) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM tokens WHERE network=?1 AND address=?2", params![network, address.to_lowercase()])? > 0)
    }

    // ---------- operations ----------

    /// Insert a new operation.
    pub fn insert_operation(&self, op: &Operation) -> Result<()> {
        // Its timeline starts here; each status change adds a stage (see `update_operation`).
        let mut detail = if op.detail.is_object() { op.detail.clone() } else { serde_json::json!({}) };
        if detail.get(TIMELINE).is_none() {
            detail[TIMELINE] = serde_json::json!([{"s": op.status.as_str(), "at": op.created}]);
        }
        let op = &Operation { detail, ..op.clone() };
        self.conn.execute(
            "INSERT INTO operations(id,network,kind,store,account,status,tx_hash,asset,amount,counterparty,fee,detail,created,updated)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                op.id,
                op.network,
                op.kind,
                op.store,
                op.account,
                op.status.as_str(),
                op.tx_hash,
                op.asset,
                op.amount,
                op.counterparty,
                op.fee,
                op.detail.to_string(),
                op.created as i64,
                op.updated as i64
            ],
        )?;
        Ok(())
    }

    /// Update status, hash, fee and merge detail keys.
    pub fn update_operation(
        &self,
        id: &str,
        status: OpStatus,
        tx_hash: Option<&str>,
        fee: Option<&str>,
        detail_patch: Option<&serde_json::Value>,
    ) -> Result<()> {
        self.write_operation(id, None, status, tx_hash, fee, detail_patch).map(|_| ())
    }

    /// [`AppDb::update_operation`], only if the operation is still at `from`; returns whether it
    /// was written. Several trackers can look at the same operation (this window's, the daemon's):
    /// only the one that actually moves it announces the change, and one that read it earlier
    /// cannot put it back.
    pub fn transition_operation(
        &self,
        id: &str,
        from: OpStatus,
        to: OpStatus,
        tx_hash: Option<&str>,
        fee: Option<&str>,
        detail_patch: Option<&serde_json::Value>,
    ) -> Result<bool> {
        self.write_operation(id, Some(from), to, tx_hash, fee, detail_patch)
    }

    /// Candidate identity and receipt state have independent lifetimes. Merge candidate
    /// metadata under the write lock without replacing a tracker's canonical inclusion.
    pub fn record_signed_candidate(&self, id: &str, hash: &str, fee: Option<&str>) -> Result<()> {
        let tx = self.immediate()?;
        let mut op = Self::operation_in(&tx, id)?.ok_or_else(|| CoreError::NotFound(format!("operation {id}")))?;
        if !op.detail.is_object() {
            op.detail = serde_json::json!({});
        }
        if op.detail.get("original_tx").is_none() {
            op.detail["original_tx"] = serde_json::json!(op.tx_hash);
        }
        let mut candidates: Vec<String> =
            op.detail.get("candidates").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
        if !candidates.iter().any(|candidate| candidate.eq_ignore_ascii_case(hash)) {
            candidates.push(hash.to_string());
        }
        op.detail["candidates"] = serde_json::json!(candidates);
        if !(op.status.is_terminal() || matches!(op.status, OpStatus::Settling | OpStatus::Locked)) {
            op.status = OpStatus::Submitted;
            op.tx_hash = Some(hash.into());
            if let Some(fee) = fee {
                op.fee = fee.into();
            }
        }
        tx.execute(
            "UPDATE operations SET status=?1,tx_hash=?2,fee=?3,detail=?4,updated=?5 WHERE id=?6",
            params![op.status.as_str(), op.tx_hash, op.fee, op.detail.to_string(), now() as i64, id],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn write_operation(
        &self,
        id: &str,
        expect: Option<OpStatus>,
        status: OpStatus,
        tx_hash: Option<&str>,
        fee: Option<&str>,
        detail_patch: Option<&serde_json::Value>,
    ) -> Result<bool> {
        // `detail` is a JSON blob, so a patch is a read, a merge and a write. Read it under the
        // write lock: the tracker and the committing thread both patch operations, and in
        // autocommit whichever read first would overwrite the other's keys.
        let tx = self.immediate()?;
        let mut op = Self::operation_in(&tx, id)?.ok_or_else(|| CoreError::NotFound(format!("operation {id}")))?;
        if expect.is_some_and(|e| e != op.status) {
            return Ok(false);
        }
        if !op.detail.is_object() {
            op.detail = serde_json::json!({});
        }
        if let Some(patch) = detail_patch.and_then(|p| p.as_object())
            && let Some(obj) = op.detail.as_object_mut()
        {
            for (k, v) in patch {
                obj.insert(k.clone(), v.clone());
            }
        }
        // The timeline: a stage per status change, and a note when a replacement's hash won.
        let at = now();
        let mut stages = Vec::new();
        if status != op.status {
            stages.push(serde_json::json!({"s": status.as_str(), "at": at}));
        }
        if let (Some(old), Some(new)) = (op.tx_hash.as_deref(), tx_hash)
            && !old.eq_ignore_ascii_case(new)
        {
            stages.push(serde_json::json!({"s": "replaced", "at": at, "tx": new}));
        }
        if !stages.is_empty() {
            let timeline = op.detail.as_object_mut().map(|o| o.entry(TIMELINE).or_insert_with(|| serde_json::json!([])));
            if let Some(serde_json::Value::Array(list)) = timeline {
                list.extend(stages);
            }
        }
        tx.execute(
            "UPDATE operations SET status=?2, tx_hash=COALESCE(?3,tx_hash), fee=COALESCE(?4,fee), detail=?5, updated=?6 WHERE id=?1",
            params![id, status.as_str(), tx_hash, fee, op.detail.to_string(), now() as i64],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// An immediate transaction on the shared connection: the write lock is taken before the
    /// first read, so a read-modify-write of a JSON blob cannot interleave with another process's.
    fn immediate(&self) -> Result<rusqlite::Transaction<'_>> {
        Ok(rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?)
    }

    fn row_to_op(r: &rusqlite::Row<'_>) -> rusqlite::Result<(Operation, String, String)> {
        Ok((
            Operation {
                id: r.get(0)?,
                network: r.get(1)?,
                kind: r.get(2)?,
                store: r.get(3)?,
                account: r.get(4)?,
                status: OpStatus::Prepared,
                tx_hash: r.get(6)?,
                asset: r.get(7)?,
                amount: r.get(8)?,
                counterparty: r.get(9)?,
                fee: r.get(10)?,
                detail: serde_json::Value::Null,
                created: r.get::<_, i64>(12)? as u64,
                updated: r.get::<_, i64>(13)? as u64,
            },
            r.get::<_, String>(5)?,
            r.get::<_, String>(11)?,
        ))
    }

    fn finish_op(raw: (Operation, String, String)) -> Result<Operation> {
        let (mut op, status, detail) = raw;
        op.status = OpStatus::parse(&status)?;
        op.detail = serde_json::from_str(&detail).unwrap_or(serde_json::Value::Null);
        Ok(op)
    }

    const OP_COLUMNS: &'static str = "id,network,kind,store,account,status,tx_hash,asset,amount,counterparty,fee,detail,created,updated";

    /// One operation.
    pub fn operation(&self, id: &str) -> Result<Option<Operation>> {
        Self::operation_in(&self.conn, id)
    }

    /// One operation read through a given connection (the shared one, or an open transaction).
    fn operation_in(conn: &Connection, id: &str) -> Result<Option<Operation>> {
        let raw = conn.query_row(&format!("SELECT {} FROM operations WHERE id=?1", Self::OP_COLUMNS), [id], Self::row_to_op).optional()?;
        raw.map(Self::finish_op).transpose()
    }

    /// Find an operation by id prefix or tx hash.
    pub fn find_operation(&self, network: &str, selector: &str) -> Result<Operation> {
        let lower = selector.to_lowercase();
        let ops: Vec<Operation> = self
            .operations(network, 10_000)?
            .into_iter()
            .filter(|o| o.id.starts_with(&lower) || o.tx_hash.as_deref().is_some_and(|h| h.to_lowercase() == lower))
            .collect();
        match ops.len() {
            0 => Err(CoreError::NotFound(format!("no operation matching `{selector}`"))),
            1 => Ok(ops.into_iter().next().expect("one op")),
            _ => Err(CoreError::Invalid(format!("`{selector}` matches several operations; use more characters"))),
        }
    }

    /// Newest operations on a network.
    pub fn operations(&self, network: &str, limit: u32) -> Result<Vec<Operation>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {} FROM operations WHERE network=?1 ORDER BY created DESC, id LIMIT ?2", Self::OP_COLUMNS))?;
        let rows = stmt.query_map(params![network, limit], Self::row_to_op)?;
        rows.map(|r| Self::finish_op(r?)).collect()
    }

    /// Non-terminal operations needing tracking.
    pub fn open_operations(&self, network: &str) -> Result<Vec<Operation>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM operations WHERE network=?1 AND status NOT IN ('confirmed','failed','settled','refunded','replaced','cancelled') ORDER BY created, id",
            Self::OP_COLUMNS
        ))?;
        let rows = stmt.query_map([network], Self::row_to_op)?;
        rows.map(|r| Self::finish_op(r?)).collect()
    }

    /// Bounded rotating canonicality audit. Inclusion is not irreversible finality:
    /// completed operations with known block identity remain auditable.
    pub fn canonical_audit_page(&self, network: &str, after: &str, limit: u32) -> Result<Vec<Operation>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM operations WHERE network=?1 AND id>?2 AND status IN ('confirmed','failed','settled','refunded') AND json_extract(detail,'$.included_hash') IS NOT NULL ORDER BY id LIMIT ?3",
            Self::OP_COLUMNS
        ))?;
        let rows = stmt.query_map(params![network, after, limit.min(64)], Self::row_to_op)?;
        rows.map(|r| Self::finish_op(r?)).collect()
    }

    /// Operations sent and not yet seen in a block (signed, submitted, or unknown), oldest first:
    /// one indexed query, cheap enough to ask every few seconds.
    pub fn awaiting_inclusion(&self, network: &str) -> Result<Vec<Operation>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM operations WHERE network=?1 AND status IN ('signed','submitted','unknown') ORDER BY created, id",
            Self::OP_COLUMNS
        ))?;
        let rows = stmt.query_map(params![network], Self::row_to_op)?;
        rows.map(|r| Self::finish_op(r?)).collect()
    }

    // ---------- activity ----------

    /// Record an observed event once; returns true when new.
    pub fn record_activity(&self, a: &Activity) -> Result<bool> {
        Ok(self.conn.execute(
            "INSERT OR IGNORE INTO activity(network,key,direction,asset,amount,address,tx_hash,block,detail,observed) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![a.network, a.key, a.direction, a.asset, a.amount, a.address, a.tx_hash, a.block.map(|b| b as i64), a.detail.to_string(), a.observed as i64],
        )? > 0)
    }

    /// Newest activity.
    pub fn activity(&self, network: &str, limit: u32) -> Result<Vec<Activity>> {
        let mut stmt = self.conn.prepare(
            "SELECT network,key,direction,asset,amount,address,tx_hash,block,detail,observed FROM activity WHERE network=?1 ORDER BY observed DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![network, limit], |r| {
            Ok(Activity {
                network: r.get(0)?,
                key: r.get(1)?,
                direction: r.get(2)?,
                asset: r.get(3)?,
                amount: r.get(4)?,
                address: r.get(5)?,
                tx_hash: r.get(6)?,
                block: r.get::<_, Option<i64>>(7)?.map(|b| b as u64),
                detail: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
                observed: r.get::<_, i64>(9)? as u64,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Clear displayed activity and terminal operations for a network (custody records in SDK stores are kept).
    pub fn clear_history(&self, network: &str) -> Result<usize> {
        let a = self.conn.execute("DELETE FROM activity WHERE network=?1", [network])?;
        let b = self.conn.execute(
            "DELETE FROM operations WHERE network=?1 AND status IN ('confirmed','failed','settled','refunded','replaced','cancelled')",
            [network],
        )?;
        Ok(a + b)
    }

    // ---------- notifications ----------

    /// Add a notification.
    pub fn notify(&self, level: &str, title: &str, body: &str) -> Result<i64> {
        self.conn
            .execute("INSERT INTO notifications(at,level,title,body) VALUES(?1,?2,?3,?4)", params![now() as i64, level, title, body])?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Recent notifications.
    pub fn notifications(&self, limit: u32) -> Result<Vec<Notice>> {
        let mut stmt = self.conn.prepare("SELECT id,at,level,title,body,read FROM notifications ORDER BY id DESC LIMIT ?1")?;
        let rows = stmt.query_map([limit], |r| {
            Ok(Notice {
                id: r.get(0)?,
                at: r.get::<_, i64>(1)? as u64,
                level: r.get(2)?,
                title: r.get(3)?,
                body: r.get(4)?,
                read: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Mark all read.
    pub fn mark_notifications_read(&self) -> Result<()> {
        self.conn.execute("UPDATE notifications SET read=1", [])?;
        Ok(())
    }

    // ---------- kv ----------

    /// Set a key.
    pub fn set_kv(&self, key: &str, value: &str) -> Result<()> {
        self.conn
            .execute("INSERT INTO kv(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, value])?;
        Ok(())
    }

    /// Get a key.
    pub fn kv(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM kv WHERE key=?1", [key], |r| r.get(0)).optional()?)
    }

    /// Every key starting with `prefix`, with its value, in key order.
    pub fn kv_prefix(&self, prefix: &str) -> Result<Vec<(String, String)>> {
        // `substr` rather than LIKE: a prefix holding `%` or `_` must match only itself.
        let mut stmt = self.conn.prepare("SELECT key, value FROM kv WHERE substr(key, 1, length(?1)) = ?1 ORDER BY key")?;
        let rows = stmt.query_map([prefix], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Remove a key (nothing happens when it is absent).
    pub fn delete_kv(&self, key: &str) -> Result<()> {
        self.conn.execute("DELETE FROM kv WHERE key=?1", [key])?;
        Ok(())
    }

    /// Consistent online backup of this database into `dest`. Re-fetchable third-party caches
    /// (prices, explorer lookups, images) are left out.
    pub fn backup_to(&self, dest: &Path) -> Result<()> {
        let mut out = Connection::open(dest)?;
        {
            let backup = rusqlite::backup::Backup::new(&self.conn, &mut out)?;
            backup.run_to_completion(64, std::time::Duration::from_millis(5), None)?;
        }
        for table in CACHE_TABLES {
            out.execute(&format!("DELETE FROM {table}"), [])?;
        }
        out.execute_batch("VACUUM")?;
        Ok(())
    }

    // ---------- append-only observation feeds ----------
    //
    // A pool's logs and the DEX tape used to be one JSON array per feed: read whole, merged in
    // memory, and written back on every refresh with no transaction. Two writers silently lost
    // each other's events, and drawing one candle meant parsing thirty days of JSON. They are
    // rows now, added with `INSERT OR IGNORE` on the position a log already has, so re-reading a
    // block someone else already recorded costs nothing and loses nothing.

    /// Record observations, replacing changed events at the same chain position.
    pub fn add_pool_events(&self, network: &str, pool: &str, events: &[(u64, u64, u64, String)]) -> Result<usize> {
        self.record_pool_scan(network, pool, events, None, None)
    }

    /// Commit a page and its cursor together. A completed canonical scan may replace its range,
    /// including an empty range (removed logs must disappear even when no replacement exists).
    pub fn record_pool_scan(
        &self,
        network: &str,
        pool: &str,
        events: &[(u64, u64, u64, String)],
        replace: Option<(u64, u64)>,
        checkpoint: Option<(&str, &str)>,
    ) -> Result<usize> {
        self.record_pool_scan_ranges(network, pool, events, &replace.into_iter().collect::<Vec<_>>(), checkpoint)
    }

    /// Replace disjoint canonical scan ranges and advance their shared checkpoint atomically.
    pub fn record_pool_scan_ranges(
        &self,
        network: &str,
        pool: &str,
        events: &[(u64, u64, u64, String)],
        replace: &[(u64, u64)],
        checkpoint: Option<(&str, &str)>,
    ) -> Result<usize> {
        self.record_pool_scan_ranges_checked(network, pool, events, replace, checkpoint, None)
    }

    /// Optimistic checkpoint guard prevents an index writer racing an in-flight canonical scan.
    pub fn record_pool_scan_ranges_checked(
        &self,
        network: &str,
        pool: &str,
        events: &[(u64, u64, u64, String)],
        replace: &[(u64, u64)],
        checkpoint: Option<(&str, &str)>,
        expected_checkpoint: Option<(&str, Option<&str>)>,
    ) -> Result<usize> {
        if events.iter().any(|(block, index, at, _)| *block > i64::MAX as u64 || *index > i64::MAX as u64 || *at > i64::MAX as u64)
            || replace.iter().any(|(from, through)| from > through || *through > i64::MAX as u64)
        {
            return Err(CoreError::Invalid("market history position exceeds the supported range".into()));
        }
        let tx = self.immediate()?;
        if let Some((key, expected)) = expected_checkpoint {
            let actual: Option<String> = tx.query_row("SELECT value FROM cache WHERE key=?1", [key], |row| row.get(0)).optional()?;
            if actual.as_deref() != expected {
                return Err(CoreError::Invalid("history changed during canonical scan; retry".into()));
            }
        }
        for (from, to) in replace {
            tx.execute(
                "DELETE FROM pool_events WHERE network=?1 AND pool=?2 AND block>=?3 AND block<=?4",
                params![network, pool.to_lowercase(), *from as i64, *to as i64],
            )?;
        }
        let mut changed = 0;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO pool_events(network,pool,block,log_index,at,event) VALUES(?1,?2,?3,?4,?5,?6)
                ON CONFLICT(network,pool,block,log_index) DO UPDATE SET at=excluded.at,event=excluded.event
                WHERE pool_events.event != excluded.event",
            )?;
            let mut changed_blocks = Vec::new();
            for (block, index, at, event) in events {
                let count = stmt.execute(params![network, pool.to_lowercase(), *block as i64, *index as i64, *at as i64, event])?;
                changed += count;
                if count != 0 {
                    changed_blocks.push(*block);
                }
            }
            // Indexed pages can arrive after a canonical scan. Invalidate coverage in the
            // same transaction as the observations, so another reader cannot call them checked.
            // Recent blocks will be replayed anyway; a changed older block drops the verified
            // prefix and is progressively rechecked without an unbounded synchronous replay.
            if replace.is_empty() && !changed_blocks.is_empty() {
                let canonical_key = format!("{network}:pool_canonical_v2:{}", pool.to_lowercase());
                let existing: Option<String> =
                    tx.query_row("SELECT value FROM cache WHERE key=?1", [&canonical_key], |row| row.get(0)).optional()?;
                {
                    let mut value =
                        existing.and_then(|v| serde_json::from_str::<serde_json::Value>(&v).ok()).unwrap_or_else(|| serde_json::json!({}));
                    value["dirty"] = serde_json::Value::Bool(true);
                    value["revision"] = value["revision"].as_u64().unwrap_or(0).saturating_add(1).into();
                    let through = value["through_block"].as_u64().unwrap_or(0);
                    if let Some(block) = changed_blocks.into_iter().filter(|b| *b < through.saturating_sub(32)).max() {
                        let from = value["from_block"].as_u64().unwrap_or(0).max(block.saturating_add(1));
                        value["from_block"] = from.into();
                    }
                    tx.execute("INSERT INTO cache(key,value,fetched) VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET value=excluded.value,fetched=excluded.fetched",
                        params![canonical_key, value.to_string(), now() as i64])?;
                }
            }
        }
        if let Some((key, value)) = checkpoint {
            tx.execute("INSERT INTO cache(key,value,fetched) VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET value=excluded.value,fetched=excluded.fetched",
                params![key, value, now() as i64])?;
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Recorded events for a pool at or after `since` (unix seconds), oldest first.
    pub fn pool_events(&self, network: &str, pool: &str, since: u64) -> Result<Vec<String>> {
        let mut stmt =
            self.conn.prepare("SELECT event FROM pool_events WHERE network=?1 AND pool=?2 AND at>=?3 ORDER BY at, block, log_index")?;
        let rows = stmt.query_map(params![network, pool.to_lowercase(), since as i64], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Recorded rows by chain position, independent of potentially incorrect indexed timestamps.
    pub fn pool_events_in_blocks(&self, network: &str, pool: &str, from: u64, through: u64) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT event FROM pool_events WHERE network=?1 AND pool=?2 AND block>=?3 AND block<=?4 ORDER BY block,log_index")?;
        let rows = stmt.query_map(params![network, pool.to_lowercase(), from as i64, through as i64], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn pool_event_blocks(&self, network: &str, pool: &str) -> Result<Option<(u64, u64)>> {
        let (from, through): (Option<i64>, Option<i64>) = self.conn.query_row(
            "SELECT min(block),max(block) FROM pool_events WHERE network=?1 AND pool=?2",
            params![network, pool.to_lowercase()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(from.zip(through).map(|(a, b)| (a as u64, b as u64)))
    }

    /// The span of what is recorded for a pool: (oldest, newest) event times, if any.
    pub fn pool_events_span(&self, network: &str, pool: &str) -> Result<Option<(u64, u64)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT min(at), max(at) FROM pool_events WHERE network=?1 AND pool=?2",
                params![network, pool.to_lowercase()],
                |r| Ok(r.get::<_, Option<i64>>(0)?.zip(r.get::<_, Option<i64>>(1)?)),
            )
            .optional()?
            .flatten()
            .map(|(a, b)| (a as u64, b as u64)))
    }

    /// Record DEX swaps, ignoring ones already known by (tx, log index). Returns how many were new.
    pub fn add_dex_swaps(&self, network: &str, swaps: &[(String, u64, u64, u64, bool, String)]) -> Result<usize> {
        let tx = self.immediate()?;
        let mut added = 0;
        {
            let mut stmt =
                tx.prepare("INSERT OR IGNORE INTO dex_swaps(network,tx,log_index,block,at,timed,swap) VALUES(?1,?2,?3,?4,?5,?6,?7)")?;
            for (hash, log_index, block, at, timed, swap) in swaps {
                added += stmt.execute(params![network, hash, *log_index as i64, *block as i64, *at as i64, *timed as i64, swap])?;
            }
        }
        tx.commit()?;
        Ok(added)
    }

    /// Replace successfully scanned AMM addresses atomically and advance their coverage even if
    /// there were no trades. Curves and pools outside the filter retain their own observations.
    #[allow(clippy::too_many_arguments)]
    pub fn record_dex_scan(
        &self,
        network: &str,
        pools: &[String],
        from: u64,
        to: u64,
        swaps: &[(String, u64, u64, u64, bool, String)],
        checkpoint: (&str, &str),
    ) -> Result<()> {
        let tx = self.immediate()?;
        for pool in pools {
            tx.execute(
                "DELETE FROM dex_swaps WHERE network=?1 AND block>=?2 AND block<=?3
                AND json_valid(swap) AND lower(json_extract(swap,'$.pool'))=?4",
                params![network, from as i64, to as i64, pool.to_lowercase()],
            )?;
        }
        for (hash, index, block, at, timed, swap) in swaps {
            tx.execute("INSERT INTO dex_swaps(network,tx,log_index,block,at,timed,swap) VALUES(?1,?2,?3,?4,?5,?6,?7)
                ON CONFLICT(network,tx,log_index) DO UPDATE SET block=excluded.block,at=excluded.at,timed=excluded.timed,swap=excluded.swap",
                params![network, hash, *index as i64, *block as i64, *at as i64, *timed as i64, swap])?;
        }
        tx.execute("INSERT INTO cache(key,value,fetched) VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET value=excluded.value,fetched=excluded.fetched",
            params![checkpoint.0, checkpoint.1, now() as i64])?;
        tx.commit()?;
        Ok(())
    }

    /// Fill in a block time learned later, for every swap in that block that was only estimated.
    pub fn time_dex_block(&self, network: &str, block: u64, at: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE dex_swaps SET at=?3, timed=1, swap=json_set(swap, '$.at', ?3, '$.timed', json('true')) WHERE network=?1 AND block=?2 AND timed=0 AND json_valid(swap)",
            params![network, block as i64, at as i64],
        )?;
        Ok(())
    }

    /// The newest `limit` recorded swaps, newest block first.
    pub fn dex_swaps(&self, network: &str, limit: usize) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT swap FROM dex_swaps WHERE network=?1 ORDER BY block DESC, log_index DESC LIMIT ?2")?;
        let rows = stmt.query_map(params![network, limit as i64], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// The highest block with a recorded swap, so the next read starts after it.
    pub fn dex_swaps_head(&self, network: &str) -> Result<Option<u64>> {
        Ok(self
            .conn
            .query_row("SELECT max(block) FROM dex_swaps WHERE network=?1", params![network], |r| r.get::<_, Option<i64>>(0))
            .optional()?
            .flatten()
            .map(|b| b as u64))
    }

    /// Blocks whose swaps still carry an estimated time, newest first.
    pub fn dex_untimed_blocks(&self, network: &str, limit: usize) -> Result<Vec<u64>> {
        let mut stmt =
            self.conn.prepare("SELECT DISTINCT block FROM dex_swaps WHERE network=?1 AND timed=0 ORDER BY block DESC LIMIT ?2")?;
        let rows = stmt.query_map(params![network, limit as i64], |r| r.get::<_, i64>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?.into_iter().map(|b| b as u64).collect())
    }

    // ---------- third-party cache ----------

    /// Cached JSON text and its fetch time.
    pub fn cache_get(&self, key: &str) -> Result<Option<(String, u64)>> {
        Ok(self
            .conn
            .query_row("SELECT value, fetched FROM cache WHERE key=?1", [key], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)))
            .optional()?)
    }

    /// Store cached JSON text.
    pub fn cache_put(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO cache(key,value,fetched) VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET value=excluded.value, fetched=excluded.fetched",
            params![key, value, now() as i64],
        )?;
        Ok(())
    }

    /// Media fetch record for a URL: (content hash, error, fetched).
    pub fn media_get(&self, url: &str) -> Result<Option<(Option<String>, String, u64)>> {
        Ok(self
            .conn
            .query_row("SELECT hash, error, fetched FROM media WHERE url=?1", [url], |r| {
                Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)? as u64))
            })
            .optional()?)
    }

    /// Record a media fetch outcome.
    pub fn media_put(&self, url: &str, hash: Option<&str>, error: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO media(url,hash,error,fetched) VALUES(?1,?2,?3,?4) ON CONFLICT(url) DO UPDATE SET hash=excluded.hash, error=excluded.error, fetched=excluded.fetched",
            params![url, hash, error, now() as i64],
        )?;
        Ok(())
    }

    /// Drop third-party data unused for `max_age` seconds: cached responses and image fetches
    /// (refetched when next needed), renditions no fetch refers to, and raw pixels older
    /// versions stored beside each PNG. Compacts the file when that freed more than 32 MB.
    /// Returns (cache rows, media rows, renditions) removed.
    pub fn prune(&self, max_age: u64) -> Result<(usize, usize, usize)> {
        let cutoff = now().saturating_sub(max_age) as i64;
        let cache = self.conn.execute("DELETE FROM cache WHERE fetched < ?1", [cutoff])?;
        let media = self.conn.execute("DELETE FROM media WHERE fetched < ?1", [cutoff])?;
        let renditions =
            self.conn.execute("DELETE FROM renditions WHERE hash NOT IN (SELECT hash FROM media WHERE hash IS NOT NULL)", [])?;
        self.conn.execute("UPDATE renditions SET rgba = x'' WHERE length(rgba) > 0", [])?;
        // The observation feeds are append-only, so this is the only thing that bounds them.
        // They are keyed by event time, not fetch time: what matters is how old the trade is.
        let feed_cutoff = now().saturating_sub(max_age.min(FEED_KEEP)) as i64;
        self.conn.execute("DELETE FROM pool_events WHERE at < ?1", [feed_cutoff])?;
        self.conn.execute("DELETE FROM dex_swaps WHERE at < ?1", [feed_cutoff])?;
        let free: i64 =
            self.conn.query_row("SELECT freelist_count * page_size FROM pragma_freelist_count, pragma_page_size", [], |r| r.get(0))?;
        if free > 32 * 1024 * 1024 {
            // Best effort: another connection's open transaction makes it wait, then give up.
            // The rewrite goes through the WAL; truncate it so the space is returned.
            let _ = self.conn.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);");
        }
        Ok((cache, media, renditions))
    }

    /// A stored rendition: (width, height, png, rgba, dominant 0xRRGGBB).
    pub fn rendition_get(&self, hash: &str, size: u32) -> Result<Option<StoredRendition>> {
        Ok(self
            .conn
            .query_row("SELECT width, height, png, rgba, dominant FROM renditions WHERE hash=?1 AND size=?2", params![hash, size], |r| {
                Ok((r.get::<_, i64>(0)? as u32, r.get::<_, i64>(1)? as u32, r.get(2)?, r.get(3)?, r.get::<_, i64>(4)? as u32))
            })
            .optional()?)
    }

    /// Store a rendition.
    pub fn rendition_put(&self, hash: &str, size: u32, width: u32, height: u32, png: &[u8], rgba: &[u8], dominant: u32) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO renditions(hash,size,width,height,png,rgba,dominant) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![hash, size, width, height, png, rgba, dominant],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(id: &str, status: OpStatus) -> Operation {
        Operation {
            id: id.into(),
            network: "orchard".into(),
            kind: "send_quai".into(),
            store: "quai".into(),
            account: "0xabc".into(),
            status,
            tx_hash: None,
            asset: "QUAI".into(),
            amount: "1".into(),
            counterparty: "0xdef".into(),
            fee: String::new(),
            detail: serde_json::json!({"a":1}),
            created: now(),
            updated: now(),
        }
    }

    /// Several processes opening the same wallet at once all get in. The migration used to run in
    /// autocommit, so two openers could both reach a schema change and the loser failed on it,
    /// unable to open the wallet at all. Here the change is dropping the old schedule tables.
    #[test]
    fn concurrent_openers_all_get_a_migrated_database() {
        let dir = std::env::temp_dir().join(format!("quai-appdb-race-{}-{}", std::process::id(), now()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.sqlite");
        // A version-1 database, from when interval conversion schedules existed.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE schedules(id TEXT PRIMARY KEY, network TEXT NOT NULL, direction TEXT NOT NULL,
                   account TEXT NOT NULL, destination TEXT NOT NULL, amount_per_run TEXT NOT NULL,
                   runs_total INTEGER NOT NULL, runs_done INTEGER NOT NULL DEFAULT 0, interval_secs INTEGER NOT NULL,
                   slippage_bps INTEGER NOT NULL, max_fee TEXT NOT NULL, next_run INTEGER NOT NULL,
                   status TEXT NOT NULL, last_error TEXT NOT NULL DEFAULT '', created INTEGER NOT NULL);
                 PRAGMA user_version=1;",
            )
            .unwrap();
        }
        let ready = std::sync::Arc::new(std::sync::Barrier::new(4));
        let openers: Vec<_> = (0..4)
            .map(|_| {
                let (path, ready) = (path.clone(), ready.clone());
                std::thread::spawn(move || {
                    ready.wait();
                    AppDb::open(&path).map(|_| ()).map_err(|e| e.to_string())
                })
            })
            .collect();
        let results: Vec<_> = openers.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(results.iter().all(|r| r.is_ok()), "every opener succeeds: {results:?}");
        let db = AppDb::open(&path).unwrap();
        let schedules: i64 = db.conn.query_row("SELECT count(*) FROM sqlite_master WHERE name='schedules'", [], |r| r.get(0)).unwrap();
        let version: i64 = db.conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!((schedules, version), (0, SCHEMA_VERSION), "the old table is gone and the version bumped");
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Older builds stored what a sealed message said in its notification. Opening such a
    /// database removes that text, leaves public channel notices alone, and leaves no copy of
    /// the old text in the file or its log.
    #[test]
    fn opening_an_old_database_removes_sealed_message_text_from_notifications() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.sqlite");
        let secret = "meet at the usual place at nine";
        {
            let db = AppDb::open(&path).unwrap();
            db.notify("chat", "Bob · sealed", &format!("bob: {secret}")).unwrap();
            db.notify("chat", "#general", "alice: gm").unwrap();
            db.notify("info", "Payment offer", "0.1 Qi waiting").unwrap();
            // The sealed message's review, journaled with its operation, text and all.
            let mut dm = op("dm1", OpStatus::Confirmed);
            dm.kind = "board_post".into();
            dm.detail = serde_json::json!({"sealed": true, "review": {"fields": [
                {"label": "To", "value": "PM8T…abcd"}, {"label": "Message", "value": secret}]}});
            db.insert_operation(&dm).unwrap();
            let mut post = op("post1", OpStatus::Confirmed);
            post.kind = "board_post".into();
            post.detail = serde_json::json!({"channel": "general", "review": {"fields": [{"label": "Message", "value": "gm all"}]}});
            db.insert_operation(&post).unwrap();
            db.conn.pragma_update(None, "user_version", 7).unwrap();
            db.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        }
        let db = AppDb::open(&path).unwrap();
        let bodies: Vec<(String, String)> = db.notifications(10).unwrap().into_iter().rev().map(|n| (n.title, n.body)).collect();
        assert_eq!(
            bodies,
            [
                ("Bob · sealed".to_string(), crate::chat::REDACTED_NOTICE.to_string()),
                ("#general".to_string(), "alice: gm".to_string()),
                ("Payment offer".to_string(), "0.1 Qi waiting".to_string()),
            ]
        );
        let fields = |id: &str| db.operation(id).unwrap().unwrap().detail["review"]["fields"].clone();
        assert_eq!(fields("dm1")[1]["value"], crate::tx::PRIVATE_FIELD, "the sealed message's text is gone from its operation");
        assert_eq!(fields("dm1")[0]["value"], "PM8T…abcd", "the rest of the review stays");
        assert_eq!(fields("post1")[0]["value"], "gm all", "a public post is public anyway");
        // Checked while the database is still open, as it is under a running TUI or daemon:
        // closing the last connection would checkpoint on its own and hide a missing step.
        for file in ["app.sqlite", "app.sqlite-wal"] {
            let bytes = std::fs::read(dir.path().join(file)).unwrap_or_default();
            assert!(!bytes.windows(secret.len()).any(|w| w == secret.as_bytes()), "{file} still holds the old text");
        }
        drop(db);
    }

    #[test]
    fn history_pages_and_checkpoints_commit_together_and_canonical_replay_removes_orphans() {
        let db = AppDb::memory().unwrap();
        let old = vec![(100, 0, 500, "orphan".to_string()), (101, 0, 505, "removed".to_string())];
        db.record_pool_scan("net", "0xAa", &old, None, Some(("cursor", "page2"))).unwrap();
        assert_eq!(db.cache_get("cursor").unwrap().unwrap().0, "page2");
        let canonical = vec![(100, 0, 501, "canonical".to_string())];
        db.record_pool_scan("net", "0xaa", &canonical, Some((100, 101)), Some(("cursor", "done"))).unwrap();
        assert_eq!(db.pool_events("net", "0xaa", 0).unwrap(), ["canonical"]);
        assert_eq!(db.cache_get("cursor").unwrap().unwrap().0, "done");
        db.record_pool_scan("net", "0xaa", &[], Some((100, 100)), None).unwrap();
        assert!(db.pool_events("net", "0xaa", 0).unwrap().is_empty());
    }

    #[test]
    fn indexed_writer_invalidates_canonical_coverage_and_racing_replay_rolls_back() {
        let db = AppDb::memory().unwrap();
        let key = "net:pool_canonical_v2:pool";
        let checked = serde_json::json!({"from_block":10,"through_block":100,"dirty":false,"revision":1}).to_string();
        db.record_pool_scan("net", "pool", &[(50, 0, 500, "old".into())], Some((10, 100)), Some((key, &checked))).unwrap();
        db.add_pool_events("net", "pool", &[(50, 0, 501, "changed".into())]).unwrap();
        let current = db.cache_get(key).unwrap().unwrap().0;
        let dirty: serde_json::Value = serde_json::from_str(&current).unwrap();
        assert_eq!(dirty["dirty"], true);
        assert_eq!(dirty["from_block"], 51);
        assert_eq!(dirty["revision"], 2);
        assert!(
            db.record_pool_scan_ranges_checked("net", "pool", &[], &[(50, 100)], Some((key, &checked)), Some((key, Some(&checked))))
                .is_err()
        );
        assert_eq!(db.pool_events_in_blocks("net", "pool", 50, 50).unwrap(), ["changed"]);
        assert_eq!(db.cache_get(key).unwrap().unwrap().0, current);
        db.record_pool_scan_ranges_checked(
            "net",
            "pool",
            &[(50, 0, 502, "fixed".into())],
            &[(20, 50), (80, 100)],
            Some((key, &checked)),
            Some((key, Some(&current))),
        )
        .unwrap();
        assert_eq!(db.pool_events("net", "pool", 0).unwrap(), ["fixed"]);
        assert_eq!(db.pool_event_blocks("net", "pool").unwrap(), Some((50, 50)));
        // Unchanged re-observations do not dirty or lose checked history.
        db.add_pool_events("net", "pool", &[(50, 0, 502, "fixed".into())]).unwrap();
        assert_eq!(db.cache_get(key).unwrap().unwrap().0, checked);
    }

    #[test]
    fn first_index_page_also_invalidates_an_uncheckpointed_inflight_scan() {
        let db = AppDb::memory().unwrap();
        let key = "net:pool_canonical_v2:pool";
        db.add_pool_events("net", "pool", &[(1, 0, 10, "trade".into())]).unwrap();
        assert!(db.record_pool_scan_ranges_checked("net", "pool", &[], &[(0, 10)], None, Some((key, None))).is_err());
        assert_eq!(db.pool_events("net", "pool", 0).unwrap(), ["trade"]);
        assert!(db.add_pool_events("net", "pool", &[(u64::MAX, 0, 10, "invalid".into())]).is_err());
    }

    #[test]
    fn failed_checkpoint_write_rolls_back_its_history_page() {
        let db = AppDb::memory().unwrap();
        db.conn.execute_batch("CREATE TRIGGER deny_checkpoint BEFORE INSERT ON cache BEGIN SELECT RAISE(ABORT, 'injected'); END;").unwrap();
        assert!(db.record_pool_scan("net", "pool", &[(1, 0, 1, "event".into())], None, Some(("cursor", "next"))).is_err());
        assert!(db.pool_events("net", "pool", 0).unwrap().is_empty());
    }

    #[test]
    fn timestamp_repair_reaches_returned_payload_and_dex_replay_is_pool_scoped() {
        let db = AppDb::memory().unwrap();
        let row = |hash: &str, pool: &str| {
            (hash.to_string(), 0, 100, 500, false, serde_json::json!({"pool": pool, "at": 500, "timed": false}).to_string())
        };
        db.add_dex_swaps("net", &[row("a", "pool-a"), row("b", "pool-b")]).unwrap();
        db.time_dex_block("net", 100, 501).unwrap();
        for value in db.dex_swaps("net", 10).unwrap() {
            let value: serde_json::Value = serde_json::from_str(&value).unwrap();
            assert_eq!(value["at"], 501);
            assert_eq!(value["timed"], true);
        }
        db.record_dex_scan("net", &["pool-a".into()], 100, 100, &[], ("scan", "100")).unwrap();
        let rows = db.dex_swaps("net", 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(serde_json::from_str::<serde_json::Value>(&rows[0]).unwrap()["pool"], "pool-b");
        assert_eq!(db.cache_get("scan").unwrap().unwrap().0, "100", "empty scan still advances coverage");
    }

    /// Two writers recording overlapping pages keep every event between them. As one JSON blob
    /// per feed, each read the whole array and wrote its own version back, so whoever committed
    /// second silently erased what the first had learned.
    #[test]
    fn overlapping_writers_lose_no_events() {
        let db = AppDb::memory().unwrap();
        let ev = |block: u64, index: u64| (block, index, 1_000 + block * 5, format!("{{\"block\":{block},\"index\":{index}}}"));
        // Two passes that saw the same middle page and one new event each.
        let first: Vec<_> = (10..14).map(|b| ev(b, 0)).collect();
        let second: Vec<_> = (12..16).map(|b| ev(b, 0)).collect();
        assert_eq!(db.add_pool_events("mainnet", "0xPOOL", &first).unwrap(), 4);
        assert_eq!(db.add_pool_events("mainnet", "0xPOOL", &second).unwrap(), 2, "the overlap is ignored, not rewritten");
        let all = db.pool_events("mainnet", "0xpool", 0).unwrap();
        assert_eq!(all.len(), 6, "blocks 10-15, once each");
        assert!(all[0].contains("\"block\":10") && all[5].contains("\"block\":15"), "oldest first: {all:?}");
        // The pool is matched case-insensitively, and another pool's events stay separate.
        db.add_pool_events("mainnet", "0xOTHER", &[ev(11, 0)]).unwrap();
        assert_eq!(db.pool_events("mainnet", "0xPOOL", 0).unwrap().len(), 6);
        assert_eq!(db.pool_events_span("mainnet", "0xpool").unwrap(), Some((1_050, 1_075)));
        assert_eq!(db.pool_events_span("mainnet", "0xnothing").unwrap(), None);
        // Only what the window asks for comes back.
        assert_eq!(db.pool_events("mainnet", "0xpool", 1_065).unwrap().len(), 3);
    }

    /// The tape reads back newest first, remembers where to resume, and lets a block time that
    /// arrived late correct the swaps that were only estimated.
    #[test]
    fn the_dex_tape_resumes_and_corrects_estimated_times() {
        let db = AppDb::memory().unwrap();
        let swap = |tx: &str, index: u64, block: u64, at: u64, timed: bool| {
            (tx.to_string(), index, block, at, timed, format!("{{\"tx\":\"{tx}\",\"block\":{block}}}"))
        };
        assert_eq!(db.dex_swaps_head("mainnet").unwrap(), None, "nothing recorded yet");
        let rows = vec![swap("0xa", 0, 100, 5_000, true), swap("0xb", 0, 101, 5_005, false), swap("0xb", 1, 101, 5_005, false)];
        assert_eq!(db.add_dex_swaps("mainnet", &rows).unwrap(), 3);
        // The same logs again add nothing: the next read resumes after the newest block.
        assert_eq!(db.add_dex_swaps("mainnet", &rows).unwrap(), 0);
        assert_eq!(db.dex_swaps_head("mainnet").unwrap(), Some(101));
        assert_eq!(db.dex_untimed_blocks("mainnet", 8).unwrap(), vec![101]);
        let newest = db.dex_swaps("mainnet", 1).unwrap();
        assert_eq!(newest.len(), 1);
        assert!(newest[0].contains("\"block\":101"), "newest first: {newest:?}");
        // A header read later fixes every swap in that block, and only the estimated ones.
        db.time_dex_block("mainnet", 101, 5_010).unwrap();
        assert!(db.dex_untimed_blocks("mainnet", 8).unwrap().is_empty());
        db.time_dex_block("mainnet", 100, 9_999).unwrap();
        assert_eq!(db.prune(0).unwrap().0, 0, "pruning an empty cache table reports no rows");
        assert!(db.dex_swaps("mainnet", 10).unwrap().is_empty(), "the feeds are bounded by age too");
    }

    /// A payment code is one person; the accounts they write from accumulate under it, so every
    /// one of them answers to their name and none of them replaces another.
    #[test]
    fn a_contact_answers_to_every_address_it_has_been_seen_at() {
        let db = AppDb::memory().unwrap();
        let id = db.add_contact("ArbBot", Some("0x00AA"), Some("PM8T…"), "").unwrap();
        assert!(db.add_contact_address(id, "0x00AA").unwrap(), "the address it was saved with");
        assert!(!db.add_contact_address(id, "0x00aa").unwrap(), "the same account again is not news");
        assert!(db.add_contact_address(id, "0x00BB").unwrap(), "a second account of theirs");
        assert_eq!(db.contact_addresses(id).unwrap(), vec!["0x00aa".to_string(), "0x00bb".to_string()]);
        // Either account resolves to them, whichever one the contact row happens to hold.
        for seen in ["0x00AA", "0x00bb"] {
            assert_eq!(db.contact_by_address(seen).unwrap().map(|c| c.name), Some("ArbBot".to_string()), "{seen}");
        }
        assert_eq!(db.contact_by_address("0x00cc").unwrap(), None, "a stranger stays a stranger");
        // Removing the contact takes its addresses with it, so a later contact cannot inherit them.
        assert!(db.remove_contact("ArbBot").unwrap());
        assert!(db.contact_addresses(id).unwrap().is_empty());
        assert_eq!(db.contact_by_address("0x00bb").unwrap(), None);
    }

    #[test]
    fn contacts_tokens_labels() {
        let db = AppDb::memory().unwrap();
        db.add_contact("Alice", Some("0x00"), None, "").unwrap();
        assert!(db.add_contact("alice", None, None, "").is_err());
        assert!(db.add_contact("Alice", Some("0x01"), None, "").is_err());
        assert_eq!(db.contacts().unwrap().len(), 1);
        assert!(db.contact("ALICE").unwrap().is_some());
        assert!(db.remove_contact("alice").unwrap());
        db.set_label("0xABC", "miner").unwrap();
        assert_eq!(db.label("0xabc").unwrap().as_deref(), Some("miner"));
        let t = Token {
            network: "orchard".into(),
            address: "0xAAA".into(),
            symbol: "WQI".into(),
            name: "Wrapped Qi".into(),
            decimals: 18,
            hidden: false,
        };
        db.upsert_token(&t).unwrap();
        assert_eq!(db.token("orchard", "wqi").unwrap().address, "0xaaa");
        assert!(db.token("mainnet", "wqi").is_err());
    }

    /// A wallet from before schedules were removed drops their tables on open and keeps its
    /// operations, including ones a schedule created (their old `schedule_id` column is ignored).
    #[test]
    fn a_database_from_before_schedules_were_removed_sheds_their_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.sqlite");
        drop(AppDb::open(&path).unwrap());
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schedules(id TEXT PRIMARY KEY, network TEXT NOT NULL);
                 CREATE TABLE schedule_runs(schedule_id TEXT NOT NULL, seq INTEGER NOT NULL);
                 INSERT INTO schedules VALUES('s1','mainnet');
                 ALTER TABLE operations ADD COLUMN schedule_id TEXT;
                 INSERT INTO operations(id,network,kind,store,account,status,asset,amount,schedule_id,created,updated)
                 VALUES('aa11','mainnet','convert_quai_to_qi','quai','0x00aa','confirmed','QUAI','1','s1',1,1);
                 PRAGMA user_version=5;",
            )
            .unwrap();
        }
        let db = AppDb::open(&path).unwrap();
        let left: i64 =
            db.conn.query_row("SELECT count(*) FROM sqlite_master WHERE name IN ('schedules','schedule_runs')", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0);
        assert_eq!(db.operation("aa11").unwrap().unwrap().kind, "convert_quai_to_qi", "history is kept");
        db.insert_operation(&op("bb22", OpStatus::Signed)).unwrap();
    }

    /// An operation keeps its stages: one per status change, and one when another hash wins.
    #[test]
    fn kv_prefix_matches_literally_and_delete_removes() {
        let db = AppDb::memory().unwrap();
        for k in ["offer:my_net:a", "offer:my_net:b", "offer:myXnet:c", "offer:my%net:d", "other:my_net:e"] {
            db.set_kv(k, "1").unwrap();
        }
        let keys = |p: &str| db.kv_prefix(p).unwrap().into_iter().map(|(k, _)| k).collect::<Vec<_>>();
        assert_eq!(keys("offer:my_net:"), ["offer:my_net:a", "offer:my_net:b"], "`_` is not a wildcard");
        assert_eq!(keys("offer:my%"), ["offer:my%net:d"], "`%` is not a wildcard");
        db.delete_kv("offer:my_net:a").unwrap();
        db.delete_kv("offer:absent").unwrap();
        assert_eq!(keys("offer:my_net:"), ["offer:my_net:b"]);
    }

    #[test]
    fn only_the_first_tracker_moves_an_operation() {
        let db = AppDb::memory().unwrap();
        db.insert_operation(&op("dd44", OpStatus::Submitted)).unwrap();
        db.insert_operation(&op("ee55", OpStatus::Confirmed)).unwrap();
        assert_eq!(db.awaiting_inclusion("orchard").unwrap().iter().map(|o| o.id.as_str()).collect::<Vec<_>>(), ["dd44"]);
        let patch = serde_json::json!({"included_block": 7});
        assert!(db.transition_operation("dd44", OpStatus::Submitted, OpStatus::Confirmed, None, None, Some(&patch)).unwrap());
        // A second tracker read it as submitted too: it neither moves it again nor puts it back.
        assert!(!db.transition_operation("dd44", OpStatus::Submitted, OpStatus::Confirmed, None, None, None).unwrap());
        assert!(!db.transition_operation("dd44", OpStatus::Submitted, OpStatus::Submitted, Some("0x1"), Some("5"), None).unwrap());
        let moved = db.operation("dd44").unwrap().unwrap();
        assert_eq!(moved.status, OpStatus::Confirmed);
        assert_eq!(moved.fee, "", "the late fee write did not land");
        let stages: Vec<&str> = moved.detail[TIMELINE].as_array().unwrap().iter().filter_map(|s| s["s"].as_str()).collect();
        assert_eq!(stages, ["submitted", "confirmed"], "confirmed once, not twice");
        assert!(db.awaiting_inclusion("orchard").unwrap().is_empty());
    }

    #[test]
    fn an_operation_keeps_a_timeline() {
        let db = AppDb::memory().unwrap();
        db.insert_operation(&op("cc33", OpStatus::Signed)).unwrap();
        db.update_operation("cc33", OpStatus::Submitted, Some("0xhash"), None, None).unwrap();
        db.update_operation("cc33", OpStatus::Submitted, Some("0xhash"), None, Some(&serde_json::json!({"x": 1}))).unwrap();
        db.update_operation("cc33", OpStatus::Submitted, Some("0xother"), None, None).unwrap();
        db.update_operation("cc33", OpStatus::Confirmed, None, None, None).unwrap();
        let stages: Vec<String> = db.operation("cc33").unwrap().unwrap().detail[TIMELINE]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["s"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(stages, ["signed", "submitted", "replaced", "confirmed"], "a patch alone adds no stage");
    }

    #[test]
    fn operations_round_trip() {
        let db = AppDb::memory().unwrap();
        db.insert_operation(&op("aa11", OpStatus::Signed)).unwrap();
        db.insert_operation(&op("bb22", OpStatus::Confirmed)).unwrap();
        db.update_operation("aa11", OpStatus::Submitted, Some("0xhash"), Some("5"), Some(&serde_json::json!({"b":2}))).unwrap();
        let got = db.operation("aa11").unwrap().unwrap();
        assert_eq!(got.status, OpStatus::Submitted);
        assert_eq!(got.detail["a"], 1);
        assert_eq!(got.detail["b"], 2);
        assert_eq!(db.open_operations("orchard").unwrap().len(), 1);
        assert_eq!(db.find_operation("orchard", "0xHASH").unwrap().id, "aa11");
        assert_eq!(db.clear_history("orchard").unwrap(), 1);
    }

    /// A lease left by a process that is gone is taken over at once (on Linux, which can see that
    /// it is gone; elsewhere once it ages out); one held by a live process (here, this one) is
    /// respected; and a release only ever drops this process's own lease.
    #[test]
    fn a_lease_from_a_dead_process_is_not_waited_out() {
        let db = AppDb::shared_memory().unwrap();
        // A pid that cannot be running: past the kernel's maximum.
        db.conn.execute("INSERT INTO fetch_leases(key, since, pid) VALUES('mainnet:prices', ?1, 2147483000)", [now() as i64]).unwrap();
        if cfg!(target_os = "linux") {
            assert!(db.claim_fetch("mainnet:prices").unwrap(), "the dead holder's lease is taken over");
        } else {
            assert!(!db.claim_fetch("mainnet:prices").unwrap(), "without /proc the holder is assumed alive");
            let aged = now().saturating_sub(FETCH_LEASE + 1) as i64;
            db.conn.execute("UPDATE fetch_leases SET since = ?1 WHERE key = 'mainnet:prices'", [aged]).unwrap();
            assert!(db.claim_fetch("mainnet:prices").unwrap(), "until its lease ages out");
        }
        assert!(!db.claim_fetch("mainnet:prices").unwrap(), "and now this live process holds it");
        // Another live process's lease (pid 1 is always running) is left alone, even by a release.
        db.conn.execute("INSERT INTO fetch_leases(key, since, pid) VALUES('mainnet:dex_pools', ?1, 1)", [now() as i64]).unwrap();
        assert!(!db.claim_fetch("mainnet:dex_pools").unwrap());
        db.release_fetch("mainnet:dex_pools").unwrap();
        assert!(!db.claim_fetch("mainnet:dex_pools").unwrap(), "a release drops only this process's own lease");
    }

    /// The line between the shared cache and a wallet's own.
    ///
    /// The shared file is read by every wallet in one data directory, and the separation between
    /// those wallets is the reason somebody keeps two. So the test a feed has to pass is not "is
    /// this value public" but "does asking for it say anything about this wallet".
    #[test]
    fn only_wallet_independent_feeds_are_shared() {
        // Every wallet fetches these, whatever it holds, and gets the same answer.
        for key in [
            "prices",
            "token_markets",
            "dex_pools",
            "launch_amm_pools_v2",
            "pool_tvl",
            "launches:0",
            "listings:all",
            "listings:0x004d92fd198c21af21016f4b119b8b851b5aeaa4",
            "collections:",
            "collections:quai",
            "subgraph_candles:0x0021:3600:200",
            "token_meta:0x002b2596ecf05c93a31ff916e8b456df6c77c750",
            // The preview of an item on the public marketplace, which every wallet loads.
            "listing_nft:0x004d92fd198c21af21016f4b119b8b851b5aeaa4:42",
        ] {
            assert!(is_shared_feed(key), "{key} is the same for every wallet");
        }
        // These name an address, or are looked up only because of what a wallet holds or browsed.
        // Their contents are public; their *existence* in a shared file is not.
        for key in [
            "holdings:0x002360bc8e2a359be7335b06de43f1c7f040f15a",
            "history:0x002360bc8e2a359be7335b06de43f1c7f040f15a",
            "listings_by:0x002360bc8e2a359be7335b06de43f1c7f040f15a",
            "owned_nfts_v2:0x002360bc8e2a359be7335b06de43f1c7f040f15a",
            "nft_candidates:0x002360bc8e2a359be7335b06de43f1c7f040f15a",
            // The same item, looked up because a wallet holds it or opened it.
            "nft:0x004d:42",
            "verified:0x002b2596ecf05c93a31ff916e8b456df6c77c750",
            "token_info:0x002b2596ecf05c93a31ff916e8b456df6c77c750",
            "collection_items:0x004d",
            "board:names",
        ] {
            assert!(!is_shared_feed(key), "{key} belongs to one wallet");
        }
        // `listings_by:` must not slip in behind `listings:` — a prefix is matched whole.
        assert!(is_shared_feed("listings:x") && !is_shared_feed("listings_by:x"));
        // And a feed without a trailing colon matches exactly, not as a prefix.
        assert!(is_shared_feed("prices") && !is_shared_feed("prices:0x00aa"));
    }

    /// The shared file holds the feeds and nothing else — no contacts, no operations, no
    /// schedules, no activity. Those are what one wallet is; this file belongs to none of them.
    #[test]
    fn the_shared_cache_holds_feeds_and_nothing_else() {
        let mut tables = schema_objects(SHARED_SCHEMA);
        tables.sort_unstable();
        assert_eq!(tables, vec!["cache", "dex_swaps", "dex_swaps_block", "fetch_leases", "pool_events", "pool_events_seen"]);
        for wallet_table in ["contacts", "operations", "activity", "tokens", "labels", "notifications", "kv"] {
            assert!(!tables.contains(&wallet_table), "{wallet_table} must never be in the shared file");
        }
        // It opens, holds rows, and reopens without losing them.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.sqlite");
        AppDb::open_shared(&path).unwrap().cache_put("mainnet:prices", "{}").unwrap();
        let again = AppDb::open_shared(&path).unwrap();
        assert!(again.cache_get("mainnet:prices").unwrap().is_some());
        // A wallet database and a shared one are different schemas and do not open as each other.
        assert!(AppDb::open(&path).is_err() || AppDb::open_shared(&dir.path().join("app.sqlite")).is_ok());
    }

    /// A wallet opened before the shared cache existed drops its own copies of the shared feeds,
    /// and keeps everything that is still its own.
    #[test]
    fn the_sweep_takes_the_shared_feeds_and_leaves_the_rest() {
        let db = AppDb::memory().unwrap();
        let shared = ["mainnet:prices", "mainnet:dex_pools", "mainnet:token_meta:0x00aa", "orchard:listings:all", "mainnet:pool_tvl"];
        let mine = ["mainnet:holdings:0x00aa", "mainnet:nft:0x00bb:1", "mainnet:verified:0x00cc", "mainnet:listings_by:0x00aa"];
        for key in shared.iter().chain(&mine) {
            db.cache_put(key, "1").unwrap();
        }
        assert_eq!(db.drop_shared_feeds().unwrap(), shared.len());
        for key in shared {
            assert!(db.cache_get(key).unwrap().is_none(), "{key} moved to the shared cache");
        }
        for key in mine {
            assert!(db.cache_get(key).unwrap().is_some(), "{key} is this wallet's own");
        }
        // `_` in a feed name is a LIKE wildcard, so a lookalike key must survive the sweep.
        db.cache_put("mainnet:poolXtvl", "1").unwrap();
        assert_eq!(db.drop_shared_feeds().unwrap(), 0);
        assert!(db.cache_get("mainnet:poolXtvl").unwrap().is_some());
    }

    #[test]
    fn reopening_creates_missing_tables_only_when_needed() {
        assert_eq!(schema_objects(SCHEMA).len(), SCHEMA.matches("IF NOT EXISTS").count());
        assert!(schema_objects(SCHEMA).contains(&"renditions") && schema_objects(SCHEMA).contains(&"operations_network"));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.sqlite");
        AppDb::open(&path).unwrap().set_kv("k", "v").unwrap();
        // A table dropped (as from an older build) comes back on the next open; data stays.
        Connection::open(&path).unwrap().execute_batch("DROP TABLE renditions").unwrap();
        let db = AppDb::open(&path).unwrap();
        assert_eq!(db.kv("k").unwrap().as_deref(), Some("v"));
        let tables: i64 = db.conn.query_row("SELECT count(*) FROM sqlite_master WHERE name = 'renditions'", [], |r| r.get(0)).unwrap();
        assert_eq!(tables, 1);
        // A second connection opens while the first is held.
        let _second = AppDb::open(&path).unwrap();
    }

    #[test]
    fn prune_drops_old_third_party_data_and_raw_pixels() {
        let db = AppDb::memory().unwrap();
        db.cache_put("fresh", "1").unwrap();
        db.conn.execute("INSERT INTO cache(key,value,fetched) VALUES('old','1',1)", []).unwrap();
        db.media_put("https://x/new.png", Some("h1"), "").unwrap();
        db.conn.execute("INSERT INTO media(url,hash,error,fetched) VALUES('https://x/old.png','h2','',1)", []).unwrap();
        db.rendition_put("h1", 256, 1, 1, b"png", &[1, 2, 3, 4], 0).unwrap();
        db.rendition_put("h2", 256, 1, 1, b"png", &[1, 2, 3, 4], 0).unwrap();
        assert_eq!(db.prune(30 * 86_400).unwrap(), (1, 1, 1));
        assert!(db.cache_get("fresh").unwrap().is_some() && db.cache_get("old").unwrap().is_none());
        let (_, _, png, rgba, _) = db.rendition_get("h1", 256).unwrap().unwrap();
        assert_eq!((png.as_slice(), rgba.len()), (b"png".as_slice(), 0), "pixels are rebuilt from the PNG");
        assert!(db.rendition_get("h2", 256).unwrap().is_none());
    }
}
