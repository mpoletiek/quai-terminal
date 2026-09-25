//! Data directory layout with owner-only permissions.

use crate::error::{CoreError, Result};
use std::path::{Path, PathBuf};

/// Environment variable overriding the data directory.
pub const HOME_ENV: &str = "QUAI_TERMINAL_HOME";

/// What the data directory was called before the wallet became Quai Terminal. Both the old
/// variable and the old directory are still honoured: someone's scripts and someone's wallets
/// outlive a rename.
pub const LEGACY_HOME_ENV: &str = "QUAI_WALLET_HOME";
const APP_DIR: &str = "quai-terminal";
const LEGACY_APP_DIR: &str = "quai-wallet";

/// Resolved application directories.
#[derive(Clone, Debug)]
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    /// Resolve from an explicit path, `QUAI_TERMINAL_HOME` (or the older `QUAI_TERMINAL_HOME`), or
    /// the platform data directory — carrying an existing wallet directory over to the new name.
    pub fn resolve(explicit: Option<PathBuf>) -> Result<Self> {
        let from_env = std::env::var_os(HOME_ENV).or_else(|| std::env::var_os(LEGACY_HOME_ENV)).filter(|v| !v.is_empty());
        let root = match (explicit, from_env) {
            (Some(path), _) => path,
            (None, Some(value)) => PathBuf::from(value),
            (None, None) => {
                let dir = |name: &str| directories::ProjectDirs::from("network", "quai", name).map(|d| d.data_dir().to_path_buf());
                let root = dir(APP_DIR).ok_or_else(|| CoreError::Storage(format!("cannot determine a data directory; set {HOME_ENV}")))?;
                if let Some(legacy) = dir(LEGACY_APP_DIR) {
                    adopt_legacy_home(&legacy, &root)?;
                }
                root
            }
        };
        let paths = Self { root };
        ensure_private_dir(&paths.root)?;
        Ok(paths)
    }

    /// Root data directory.
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// Application configuration file.
    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.toml")
    }
    /// Directory containing all wallets.
    pub fn wallets_dir(&self) -> PathBuf {
        self.root.join("wallets")
    }
    /// One wallet's directory.
    pub fn wallet_dir(&self, id: &str) -> PathBuf {
        self.wallets_dir().join(id)
    }
    /// Per-wallet, per-network state directory.
    pub fn network_dir(&self, wallet_id: &str, network_id: &str) -> PathBuf {
        self.wallet_dir(wallet_id).join("networks").join(network_id)
    }
    /// User theme directory (same schema as Omarchy `colors.toml`).
    pub fn themes_dir(&self) -> PathBuf {
        self.root.join("themes")
    }
    /// Public runtime status written by the daemon for status bars.
    pub fn status_file(&self) -> PathBuf {
        self.root.join("status.json")
    }
    /// Daemon lock file.
    ///
    /// In the runtime directory, keyed by which data directory it guards — never inside the data
    /// directory itself. A lock that lives beside the wallets travels with them: copying a
    /// `QUAI_TERMINAL_HOME` to test against, which is how this project is developed, used to carry
    /// a stale lock into the copy. The kernel also drops these on reboot, which a file in the data
    /// directory does not.
    pub fn daemon_lock(&self) -> PathBuf {
        runtime_dir().join(format!("daemon-{}.lock", dir_key(&self.root)))
    }
    /// The running daemon's control socket (unlock, lock, status, stop). Beside its lock, in the
    /// user's private runtime directory.
    pub fn daemon_socket(&self) -> PathBuf {
        socket_in(runtime_dir(), format!("daemon-{}.sock", dir_key(&self.root)))
    }
    /// The running daemon's engine socket: terminals attach here, and the engine (and the keys)
    /// run in the daemon. Beside its control socket.
    pub fn engine_socket(&self) -> PathBuf {
        socket_in(runtime_dir(), format!("engine-{}.sock", dir_key(&self.root)))
    }
    /// What the running daemon watches and which wallets it holds unlocked (public: no amounts).
    pub fn daemon_state(&self) -> PathBuf {
        runtime_dir().join(format!("daemon-{}.json", dir_key(&self.root)))
    }
    /// The background daemon's log.
    pub fn daemon_log(&self) -> PathBuf {
        self.root.join("daemon.log")
    }
    /// Directory for user-requested backups.
    pub fn backups_dir(&self) -> PathBuf {
        self.root.join("backups")
    }
    /// The shared display cache: market feeds that are the same for every wallet.
    ///
    /// At the data root rather than under a wallet, because the data is not any wallet's — prices,
    /// the pool directory, listings, candles and the DEX tape are the same URL and the same answer
    /// for everybody. Keeping it here dedupes it across every wallet and every process on one
    /// machine. Nothing address-linked may go in it: see `SharedDb`.
    pub fn shared_cache(&self) -> PathBuf {
        self.root.join("shared.sqlite")
    }
}

/// Move a wallet directory left by the old name to the new one, once.
///
/// Wallets are the one thing here that cannot be recreated, so this is deliberately timid: it
/// acts only when the new directory does not exist yet, it *renames* (atomic on one filesystem)
/// rather than copying, and any failure leaves the old directory exactly where it was and is
/// reported rather than swallowed — a wallet that cannot be found is worse than an error.
fn adopt_legacy_home(legacy: &Path, root: &Path) -> Result<()> {
    if legacy == root || root.exists() || !legacy.join("wallets").is_dir() {
        return Ok(());
    }
    if let Some(parent) = root.parent() {
        ensure_private_dir(parent)?;
    }
    std::fs::rename(legacy, root).map_err(|e| {
        CoreError::Storage(format!(
            "found wallets in {} but could not move them to {}: {e}. Move the directory yourself, or set {HOME_ENV} to the old path.",
            legacy.display(),
            root.display()
        ))
    })
}

/// Where per-boot runtime state (locks) lives: `$XDG_RUNTIME_DIR/quai-terminal`, falling back to
/// a private directory under the temporary directory when the session has no runtime directory.
pub fn runtime_dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("{APP_DIR}-{}", current_uid())));
    base.join(APP_DIR)
}

/// Longest socket path used: a socket's address holds 108 bytes on Linux and 104 on macOS.
const SOCKET_PATH_MAX: usize = 100;

/// A socket in the runtime directory — or, when that path would be too long for a socket's
/// address (macOS keeps its temporary directory deep under `/var/folders`), in a short private
/// directory under `/tmp`. The daemon and its clients compute the same path.
fn socket_in(runtime: PathBuf, name: String) -> PathBuf {
    let full = runtime.join(&name);
    if full.as_os_str().len() <= SOCKET_PATH_MAX {
        return full;
    }
    PathBuf::from("/tmp").join(format!("{APP_DIR}-{}", current_uid())).join(name)
}

/// The user this process runs as, for a temporary directory nobody else's fallback collides with.
fn current_uid() -> String {
    // `id -u` without a libc dependency: the runtime directory is the authority when it exists,
    // and this is only a fallback name.
    std::env::var("UID").ok().or_else(|| std::env::var("USER").ok()).unwrap_or_else(|| "user".into())
}

/// A short, stable name for a data directory, so one runtime directory can hold a lock per home
/// without the lock's name revealing where that home is.
fn dir_key(root: &Path) -> String {
    use sha2::Digest;
    let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let digest = sha2::Sha256::digest(canonical.as_os_str().as_encoded_bytes());
    hex::encode(&digest[..8])
}

/// Create a directory (and parents) readable only by the owner.
pub fn ensure_private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Validate a user-supplied short identifier (wallet or network ids, labels in paths).
pub fn validate_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(CoreError::Invalid(format!("identifier `{id}` must be 1-64 characters of letters, digits, '-' or '_'")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A socket must fit a socket address. macOS's temporary directory is deep enough that the
    /// runtime directory's path would not (the bind fails, and a terminal waits on a daemon
    /// that never serves): those go under `/tmp` instead, and short paths stay where they are.
    #[test]
    fn sockets_fit_a_socket_address() {
        let short = socket_in(PathBuf::from("/run/user/1000/quai-terminal"), "engine-0123456789abcdef.sock".into());
        assert_eq!(short, PathBuf::from("/run/user/1000/quai-terminal/engine-0123456789abcdef.sock"));
        let mac = PathBuf::from("/var/folders/zz/zyxvpxvq6csfxvn_n0000000000000/T/quai-terminal-someone/quai-terminal");
        let moved = socket_in(mac, "engine-0123456789abcdef.sock".into());
        assert!(moved.starts_with("/tmp"), "{}", moved.display());
        assert!(moved.as_os_str().len() <= SOCKET_PATH_MAX);
        assert!(moved.ends_with("engine-0123456789abcdef.sock"), "the name, and so the home it serves, is kept");
    }

    /// The daemon lock lives outside the data directory, and one runtime directory can hold a
    /// lock per home. Copying a `QUAI_TERMINAL_HOME` — how this project is tested — must not
    /// carry a lock into the copy.
    #[test]
    fn the_daemon_lock_is_not_inside_the_data_directory() {
        let a = Paths { root: std::env::temp_dir().join("quai-lock-a") };
        let b = Paths { root: std::env::temp_dir().join("quai-lock-b") };
        assert!(!a.daemon_lock().starts_with(a.root()), "{}", a.daemon_lock().display());
        assert_ne!(a.daemon_lock(), b.daemon_lock(), "two homes, two locks");
        assert_eq!(a.daemon_lock(), Paths { root: a.root().to_path_buf() }.daemon_lock(), "and one home, one lock");
        // The name identifies the home without spelling out where it is.
        let name = a.daemon_lock().file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("daemon-") && name.ends_with(".lock"), "{name}");
        assert!(!name.contains("quai-lock-a"), "the path is hashed, not embedded: {name}");
    }

    fn wallet_dir(root: &Path, name: &str) {
        std::fs::create_dir_all(root.join("wallets").join(name)).unwrap();
        std::fs::write(root.join("wallets").join(name).join("wallet.json"), b"{}").unwrap();
    }

    /// The rename must not cost anyone a wallet: an existing directory is carried over whole,
    /// and every case where that is not obviously safe is left alone.
    #[test]
    fn wallets_from_the_old_name_are_carried_over_once() {
        let tmp = tempfile::tempdir().unwrap();
        let (legacy, root) = (tmp.path().join(LEGACY_APP_DIR), tmp.path().join(APP_DIR));
        wallet_dir(&legacy, "main");
        adopt_legacy_home(&legacy, &root).unwrap();
        assert!(root.join("wallets/main/wallet.json").is_file(), "the wallets moved");
        assert!(!legacy.exists(), "and the old directory is gone, not duplicated");
        // Running again is a no-op rather than a second move.
        adopt_legacy_home(&legacy, &root).unwrap();
        assert!(root.join("wallets/main/wallet.json").is_file());
    }

    /// Anything ambiguous is left for the user to settle: two directories are never merged, and
    /// a directory holding no wallets is not worth moving.
    #[test]
    fn an_existing_new_directory_is_never_merged_into() {
        let tmp = tempfile::tempdir().unwrap();
        let (legacy, root) = (tmp.path().join(LEGACY_APP_DIR), tmp.path().join(APP_DIR));
        wallet_dir(&legacy, "old");
        wallet_dir(&root, "new");
        adopt_legacy_home(&legacy, &root).unwrap();
        assert!(legacy.join("wallets/old/wallet.json").is_file(), "the old directory is untouched");
        assert!(root.join("wallets/new/wallet.json").is_file(), "and so is the new one");
        // Nothing to carry over: no wallets, or the same directory under both names.
        let empty = tmp.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        adopt_legacy_home(&empty, &tmp.path().join("fresh")).unwrap();
        assert!(!tmp.path().join("fresh").exists());
        adopt_legacy_home(&legacy, &legacy).unwrap();
        assert!(legacy.join("wallets/old/wallet.json").is_file());
    }
}
