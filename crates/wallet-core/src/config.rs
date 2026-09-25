//! Application preferences (`config.toml`). Never contains secrets.

use crate::error::{CoreError, Result};
use crate::network::NetworkProfile;
use crate::paths::Paths;
use crate::registry::VaultExt;
use serde::{Deserialize, Serialize};

/// Motion preference for TUI effects.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Motion {
    /// Everything in `Full`, plus ambient light and idle motion (backlight glows in pixel terminals).
    #[default]
    Vivid,
    /// Micro-transitions and ceremony effects.
    Full,
    /// Micro-transitions only.
    Reduced,
    /// No animation.
    Off,
}

impl Motion {
    /// Transitions and ceremony effects play (`Full` and `Vivid`).
    pub fn effects(self) -> bool {
        matches!(self, Motion::Full | Motion::Vivid)
    }
}

/// Terminal graphics tier override.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphicsMode {
    /// Probe the terminal.
    #[default]
    Auto,
    /// Force pixel graphics (kitty).
    Pixels,
    /// Unicode cell graphics.
    Cells,
    /// Text only.
    Text,
}

/// How the TUI uses the mouse.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MouseMode {
    /// Full on this computer, clicks only over SSH (every pointer move would cross the network),
    /// off on the Linux console.
    #[default]
    Auto,
    /// Clicks, the wheel, drags and hover.
    Full,
    /// Clicks, the wheel and drags; no hover.
    Click,
    /// No mouse: the terminal keeps its own text selection.
    Off,
}

/// What the TUI paints behind everything.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackgroundMode {
    /// The terminal's own background (and its opacity) when the theme's background is the
    /// terminal's color, as with Omarchy themes; the theme's color otherwise.
    #[default]
    Auto,
    /// Always the terminal's own background: a translucent window stays translucent under any
    /// theme.
    Terminal,
    /// Always the theme's color: solid, whatever is behind the window.
    Solid,
}

/// Which glyphs the TUI draws.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IconMode {
    /// Nerd Font icons where the terminal draws them (kitty, ghostty, WezTerm, or an installed
    /// Nerd Font; never over SSH), Unicode elsewhere, ASCII on the Linux console.
    #[default]
    Auto,
    Nerd,
    Unicode,
    Ascii,
}

/// Current TUI layout version (sections and sub-tabs).
pub const LAYOUT_VERSION: u32 = 5;

/// What third-party data a caller may fetch, from the preferences plus `--offline-data`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataPolicy {
    /// Address-linked explorer lookups.
    pub explorer: bool,
    /// Market data (prices, collections, listings).
    pub market: bool,
    /// NFT images.
    pub images: bool,
    /// Token icons.
    pub icons: bool,
}

impl DataPolicy {
    /// Nothing third-party.
    pub const OFFLINE: DataPolicy = DataPolicy { explorer: false, market: false, images: false, icons: false };
}

/// The optional parts of the wallet. One that is off is hidden in the terminal, its commands
/// refuse to run, and nothing watches for it in the background: no daemon polling, no preloads,
/// no notifications.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Features {
    /// The message board, sealed DMs and the chat dock. Off until the messaging design settles.
    pub messaging: bool,
    /// Markets, swaps, pools, farms, launches and pair alerts. Converting and wrapping are wallet
    /// operations and stay available.
    pub trading: bool,
    /// Collected NFTs, collections and marketplace listings.
    pub nfts: bool,
}

impl Default for Features {
    fn default() -> Self {
        Self { messaging: false, trading: true, nfts: true }
    }
}

/// How much of the wallet the terminal shows, over the feature switches: Simple is the money
/// (home, send and receive, one exchange, activity, collected NFTs, contacts and messages); Pro
/// adds the trader's views (markets, pools, launches, orders, PnL), the NFT marketplace, the Qi
/// coin list, the board and the network pages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Simple,
    Pro,
}

impl Mode {
    /// A configuration from before the choice existed: everything, as it always showed.
    fn before_modes() -> Mode {
        Mode::Pro
    }

    /// `simple` or `pro`.
    pub fn key(self) -> &'static str {
        match self {
            Mode::Simple => "simple",
            Mode::Pro => "pro",
        }
    }
}

/// One optional part of the wallet ([`Features`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    Messaging,
    Trading,
    Nfts,
}

impl Feature {
    pub const ALL: [Feature; 3] = [Feature::Messaging, Feature::Trading, Feature::Nfts];

    /// The `config set features.<key>` name.
    pub fn key(self) -> &'static str {
        match self {
            Feature::Messaging => "messaging",
            Feature::Trading => "trading",
            Feature::Nfts => "nfts",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Feature::Messaging => "Messaging",
            Feature::Trading => "Trading",
            Feature::Nfts => "NFTs",
        }
    }

    /// `NFTs are turned off`.
    pub fn off_note(self) -> &'static str {
        match self {
            Feature::Messaging => "Messaging is turned off",
            Feature::Trading => "Trading is turned off",
            Feature::Nfts => "NFTs are turned off",
        }
    }

    /// Why something that needs this feature did not run, and how to turn it on.
    pub fn off_error(self) -> CoreError {
        CoreError::Invalid(format!(
            "{}. Turn it on in System › Settings, or with `quai-terminal config set features.{} on`",
            self.off_note(),
            self.key()
        ))
    }
}

impl Features {
    pub fn on(&self, feature: Feature) -> bool {
        match feature {
            Feature::Messaging => self.messaging,
            Feature::Trading => self.trading,
            Feature::Nfts => self.nfts,
        }
    }

    pub fn set(&mut self, feature: Feature, on: bool) {
        match feature {
            Feature::Messaging => self.messaging = on,
            Feature::Trading => self.trading = on,
            Feature::Nfts => self.nfts = on,
        }
    }

    /// `Ok` when `feature` is on, else the error that says how to turn it on.
    pub fn require(&self, feature: Feature) -> Result<()> {
        if self.on(feature) { Ok(()) } else { Err(feature.off_error()) }
    }
}

/// Persisted preferences.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Network selected when none is given.
    pub default_network: String,
    /// Wallet selected when none is given.
    pub default_wallet: Option<String>,
    /// Minutes of inactivity before the TUI locks. 0 disables.
    pub auto_lock_minutes: u32,
    /// Theme name, path, `auto` (Omarchy then terminal) or `terminal`.
    pub theme: String,
    /// Motion preference.
    pub motion: Motion,
    /// Graphics tier.
    pub graphics: GraphicsMode,
    /// Mouse support (`auto`, `full`, `click`, `off`).
    pub mouse: MouseMode,
    /// The page background (`auto`, `terminal`, `solid`).
    pub background: BackgroundMode,
    /// Glyphs (`auto`, `nerd`, `unicode`, `ascii`).
    pub icons: IconMode,
    /// Desktop/terminal notifications.
    pub notifications: bool,
    /// Include amounts in notifications and status output.
    pub show_amounts_in_notifications: bool,
    /// Market data: token prices, collections and floors (not tied to your addresses).
    pub fetch_prices: bool,
    /// Explorer lookups for your addresses: holdings, NFTs, value history and lockups.
    pub explorer_lookups: bool,
    /// NFT images through the explorer media proxy.
    pub images: bool,
    /// Token icons from the explorer.
    pub token_icons: bool,
    /// The one-time explorer disclosure has been shown.
    pub data_disclosure_shown: bool,
    /// Last TUI layout the user has seen (the help overlay explains changes once).
    pub layout_seen: u32,
    /// Default swap slippage in basis points.
    pub swap_slippage_bps: u16,
    /// Swap deadline in minutes.
    pub swap_deadline_minutes: u32,
    /// Play the unlock/lock-screen text effects.
    pub ceremonies: bool,
    /// Terminal bell on confirmed receipts and unlocked funds (opt-in).
    pub sound: bool,
    /// A review signs only after Enter is held until its bar fills, never on one press (opt-in).
    pub hold_to_sign: bool,
    /// h j k l move the cursor as well as the arrows. Off frees them for each view's own actions.
    pub vim_keys: bool,
    /// The lock screen plays one animation after another. Off, it plays one per lock and then
    /// rests on the still wordmark, which costs no CPU while the wallet sits locked.
    pub lock_loop: bool,
    /// Large block digits for balances (off for screen readers / NO_COLOR).
    pub big_numbers: bool,
    /// Show the wallet's QUAI balance in the top bar (`$` hides it for a shoulder-surfer).
    pub balance_in_bar: bool,
    /// Screen layout: `auto` (trader on very wide terminals), `standard`, `trader` (Markets and
    /// the swap card side by side) or `focus` (no section sidebar).
    pub layout: String,
    /// Opening the terminal starts the background daemon when it is not running, so alerts,
    /// chats and notifications carry on after it closes.
    pub daemon_autostart: bool,
    /// Unlocking a wallet in the terminal unlocks it in the running daemon too (checked hand-off
    /// over its private socket), so its sealed chats and private payments keep being read after
    /// the terminal closes. On by default, so the daemon simply keeps doing what the terminal was
    /// doing. Locking the terminal locks the daemon too.
    pub daemon_share_unlock: bool,
    /// Hours the daemon keeps a wallet unlocked before it locks it on its own. A walk-away
    /// machine should not hold a recovery phrase in a background process forever.
    pub daemon_unlock_hours: u64,
    /// Lock screen animation: `random` or a ttfx effect name (looped).
    pub lock_effect: String,
    /// Theme chosen during onboarding has been shown (first-run showroom).
    pub onboarded: bool,
    /// The one-time first-receipt celebration has played.
    pub first_receive_celebrated: bool,
    /// Optional parts of the wallet that are turned on.
    pub features: Features,
    /// How much of the wallet the terminal shows. A new configuration starts Simple; one written
    /// before there was a choice has no `mode` and stays Pro, as its owner knows it.
    #[serde(default = "Mode::before_modes")]
    pub mode: Mode,
    /// Message-board channels this wallet follows, in the order they are shown.
    #[serde(default = "default_channels")]
    pub board_channels: Vec<String>,
    /// Proxy for third-party lookups and node RPC (`socks5h://127.0.0.1:9050` for Tor). A node on
    /// this machine or the LAN is still reached directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    /// IPFS gateway for images and NFT metadata (`crate::ipfs`). None is `https://ipfs.qu.ai`.
    /// A node on this machine or the LAN may be plain http and is reached without the proxy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipfs_gateway: Option<String>,
    /// IPFS gateway for contract metadata — the ABIs a contract's bytecode names by CID. Kept
    /// apart from the one above because `https://ipfs.qu.ai`, the default, is the authority for
    /// these: it is what `pushMetadataToIPFS` pins to and what Quaiscan verifies against. Only
    /// worth changing for a node that pins Quai contract metadata itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abi_ipfs_gateway: Option<String>,
    /// User-defined networks.
    pub networks: Vec<NetworkProfile>,
    /// Monitoring endpoints by network id (built-in or custom). Once one reports the network's
    /// chain id and genesis, every blockchain read goes to it, reviews included; broadcasting
    /// always goes to the network's RPC, and a review warns when the monitor has fallen
    /// [`crate::network::MONITOR_LAG_WARN`] blocks behind that RPC.
    pub monitor_endpoints: std::collections::BTreeMap<String, crate::network::MonitorEndpoint>,
    /// Formerly an explicit opt-in for review reads from a monitoring endpoint. A configured
    /// monitor now serves them; the field is kept only so older configs still load.
    pub execution_monitor_trust: std::collections::BTreeMap<String, String>,
    /// Explicit opt-in for remote plaintext RPC on these network ids.
    pub allow_insecure_rpc: std::collections::BTreeSet<String>,
}

/// The channel a new wallet starts on.
fn default_channels() -> Vec<String> {
    vec!["general".into()]
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            default_network: "mainnet".into(),
            default_wallet: None,
            auto_lock_minutes: 10,
            theme: "auto".into(),
            motion: Motion::Vivid,
            graphics: GraphicsMode::Auto,
            mouse: MouseMode::Auto,
            background: BackgroundMode::Auto,
            icons: IconMode::Auto,
            notifications: true,
            show_amounts_in_notifications: false,
            // Address-linked lookups are opt-in: onboarding asks, and until then nothing tells a
            // third party which addresses are yours. Market data carries no addresses.
            fetch_prices: true,
            explorer_lookups: false,
            images: false,
            token_icons: false,
            data_disclosure_shown: false,
            layout_seen: 0,
            features: Features::default(),
            mode: Mode::Simple,
            board_channels: default_channels(),
            swap_slippage_bps: 50,
            swap_deadline_minutes: 10,
            ceremonies: true,
            sound: false,
            hold_to_sign: false,
            vim_keys: true,
            lock_loop: true,
            big_numbers: true,
            balance_in_bar: true,
            layout: "auto".into(),
            daemon_autostart: true,
            daemon_share_unlock: true,
            daemon_unlock_hours: 12,
            lock_effect: "random".into(),
            onboarded: false,
            first_receive_celebrated: false,
            proxy: None,
            ipfs_gateway: None,
            abi_ipfs_gateway: None,
            networks: Vec::new(),
            monitor_endpoints: std::collections::BTreeMap::new(),
            execution_monitor_trust: std::collections::BTreeMap::new(),
            allow_insecure_rpc: std::collections::BTreeSet::new(),
        }
    }
}

impl AppConfig {
    /// Whether reviews read from the network's monitoring endpoint: whenever one is configured.
    /// (Its identity is still checked before any read goes to it, and a lagging one is flagged
    /// on the review.)
    pub fn monitor_serves_execution(&self, profile: &NetworkProfile) -> bool {
        profile.monitor.is_some()
    }

    pub fn require_execution_transport(&self, profile: &NetworkProfile) -> Result<()> {
        crate::network::require_secure_rpc(&profile.rpc_url, self.allow_insecure_rpc.contains(&profile.id))?;
        if self.monitor_serves_execution(profile) {
            crate::network::require_secure_rpc(
                &profile.monitor.as_ref().expect("trusted monitor").rpc_url,
                self.allow_insecure_rpc.contains(&profile.id),
            )?;
        }
        Ok(())
    }

    /// Third-party data policy (all off when the process runs with `--offline-data`).
    pub fn data_policy(&self) -> DataPolicy {
        if crate::http::offline() {
            return DataPolicy::OFFLINE;
        }
        DataPolicy { explorer: self.explorer_lookups, market: self.fetch_prices, images: self.images, icons: self.token_icons }
    }

    /// Load configuration, returning defaults when the file does not exist.
    pub fn load(paths: &Paths) -> Result<Self> {
        let path = paths.config_file();
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| CoreError::Invalid(format!("config.toml: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomically save configuration.
    pub fn save(&self, paths: &Paths) -> Result<()> {
        let text = toml::to_string_pretty(self).map_err(|e| CoreError::Storage(format!("config encode: {e}")))?;
        wallet_vault::write_private_atomic(&paths.config_file(), text.as_bytes()).vault()?;
        Ok(())
    }

    /// All known networks: built-ins followed by user networks (user ids may not shadow built-ins).
    pub fn networks(&self) -> Vec<NetworkProfile> {
        let mut all = NetworkProfile::builtins();
        for n in &self.networks {
            if !all.iter().any(|b| b.id == n.id) {
                all.push(n.clone());
            }
        }
        for n in &mut all {
            if let Some(m) = self.monitor_endpoints.get(&n.id) {
                n.monitor = Some(m.clone());
            }
        }
        all
    }

    /// Find a network by id.
    pub fn network(&self, id: &str) -> Result<NetworkProfile> {
        self.networks().into_iter().find(|n| n.id == id).ok_or_else(|| CoreError::NotFound(format!("unknown network `{id}`")))
    }

    /// Set a key from `config set` using dotted names.
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let parse_bool = |v: &str| match v {
            "true" | "on" | "yes" | "1" => Ok(true),
            "false" | "off" | "no" | "0" => Ok(false),
            _ => Err(CoreError::Invalid(format!("expected true/false, got `{v}`"))),
        };
        match key {
            "default_network" => self.default_network = value.into(),
            "default_wallet" => self.default_wallet = (!value.is_empty()).then(|| value.to_string()),
            "auto_lock_minutes" => {
                self.auto_lock_minutes = value.parse().map_err(|_| CoreError::Invalid("auto_lock_minutes must be a number".into()))?
            }
            "theme" => self.theme = value.into(),
            "mode" => {
                self.mode = match value {
                    "simple" => Mode::Simple,
                    "pro" => Mode::Pro,
                    _ => return Err(CoreError::Invalid(format!("mode is simple or pro, not `{value}`"))),
                }
            }
            "motion" => {
                self.motion = match value {
                    "vivid" => Motion::Vivid,
                    "full" => Motion::Full,
                    "reduced" => Motion::Reduced,
                    "off" => Motion::Off,
                    _ => return Err(CoreError::Invalid("motion: vivid|full|reduced|off".into())),
                }
            }
            "graphics" => {
                self.graphics = match value {
                    "auto" => GraphicsMode::Auto,
                    "pixels" => GraphicsMode::Pixels,
                    "cells" => GraphicsMode::Cells,
                    "text" => GraphicsMode::Text,
                    _ => {
                        return Err(CoreError::Invalid("graphics: auto|pixels|cells|text".into()));
                    }
                }
            }
            "notifications" => self.notifications = parse_bool(value)?,
            "show_amounts_in_notifications" => self.show_amounts_in_notifications = parse_bool(value)?,
            "fetch_prices" | "market_data" => self.fetch_prices = parse_bool(value)?,
            "explorer_lookups" => self.explorer_lookups = parse_bool(value)?,
            "images" => self.images = parse_bool(value)?,
            "token_icons" => self.token_icons = parse_bool(value)?,
            "swap_slippage_bps" => {
                self.swap_slippage_bps = value
                    .parse()
                    .ok()
                    .filter(|v| *v <= 5000)
                    .ok_or_else(|| CoreError::Invalid("swap_slippage_bps must be 0-5000".into()))?
            }
            "swap_deadline_minutes" => {
                self.swap_deadline_minutes = value
                    .parse()
                    .ok()
                    .filter(|v| (1..=1440).contains(v))
                    .ok_or_else(|| CoreError::Invalid("swap_deadline_minutes must be 1-1440".into()))?
            }
            "ceremonies" => self.ceremonies = parse_bool(value)?,
            "sound" => self.sound = parse_bool(value)?,
            "big_numbers" => self.big_numbers = parse_bool(value)?,
            "balance_in_bar" => self.balance_in_bar = parse_bool(value)?,
            "daemon_autostart" => self.daemon_autostart = parse_bool(value)?,
            "daemon_share_unlock" => self.daemon_share_unlock = parse_bool(value)?,
            "daemon_unlock_hours" => {
                self.daemon_unlock_hours =
                    value.parse().ok().filter(|h| (1..=168).contains(h)).ok_or_else(|| CoreError::Invalid("1 to 168 hours".into()))?
            }
            "layout" => {
                if !matches!(value, "auto" | "standard" | "trader" | "focus") {
                    return Err(CoreError::Invalid("layout is auto, standard, trader or focus".into()));
                }
                self.layout = value.into();
            }
            "lock_effect" => self.lock_effect = value.into(),
            _ if key.starts_with("features.") => {
                let name = &key["features.".len()..];
                let feature = Feature::ALL
                    .into_iter()
                    .find(|f| f.key() == name)
                    .ok_or_else(|| CoreError::Invalid(format!("unknown feature `{name}` (messaging, trading, nfts)")))?;
                self.features.set(feature, parse_bool(value)?);
            }
            "proxy" => {
                let value = value.trim();
                if !value.is_empty() {
                    crate::http::validate_proxy(value)?;
                }
                self.proxy = (!value.is_empty()).then(|| value.to_string());
            }
            "ipfs_gateway" | "abi_ipfs_gateway" => {
                let content = if key == "abi_ipfs_gateway" { crate::ipfs::Content::Abi } else { crate::ipfs::Content::Media };
                let value = value.trim();
                // `default` (or nothing) goes back to the built-in gateway for that content.
                let stored = if value.is_empty() || value == "default" {
                    None
                } else {
                    let gateway = crate::ipfs::Gateway::parse(value)?;
                    (!gateway.is_default_for(content)).then(|| gateway.display())
                };
                match content {
                    crate::ipfs::Content::Abi => self.abi_ipfs_gateway = stored,
                    crate::ipfs::Content::Media => self.ipfs_gateway = stored,
                }
            }
            _ => return Err(CoreError::Invalid(format!("unknown config key `{key}`"))),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configured_monitor_serves_reviews_over_a_secure_transport() {
        let config = AppConfig::default();
        let mut profile = NetworkProfile::builtins().remove(0);
        assert!(!config.monitor_serves_execution(&profile), "no monitor, no monitor reads");
        profile.monitor = Some(crate::network::MonitorEndpoint { rpc_url: "https://node.example".into(), use_pathing: false });
        assert!(config.monitor_serves_execution(&profile), "a configured monitor serves every read");
        assert!(config.require_execution_transport(&profile).is_ok());
        // Remote plaintext still needs consent, whichever endpoint it is.
        profile.monitor.as_mut().unwrap().rpc_url = "http://public.example".into();
        assert!(config.require_execution_transport(&profile).is_err());
        profile.monitor.as_mut().unwrap().rpc_url = "http://10.0.0.12:9200".into();
        assert!(config.require_execution_transport(&profile).is_ok(), "a LAN node may be plain http");
        for url in ["https://public.example", "http://localhost:9000", "http://127.0.0.1", "http://192.168.1.2", "http://[::1]"] {
            assert!(crate::network::require_secure_rpc(url, false).is_ok(), "{url}");
        }
        assert!(crate::network::require_secure_rpc("http://public.example", false).is_err());
        assert!(crate::network::require_secure_rpc("http://8.8.8.8", false).is_err());
        assert!(crate::network::require_secure_rpc("http://public.example", true).is_ok());
        assert_eq!(crate::network::rpc_origin("https://user:secret@public.example/key?token=secret"), "https://public.example");
    }

    #[test]
    fn roundtrip_and_set() {
        let mut c = AppConfig::default();
        c.set("motion", "off").unwrap();
        c.set("notifications", "false").unwrap();
        assert!(c.set("bogus", "1").is_err());
        let text = toml::to_string_pretty(&c).unwrap();
        let back: AppConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.motion, Motion::Off);
        assert!(!back.notifications);
        assert!(back.network("mainnet").is_ok());
        assert!(back.network("orchard").is_ok());
    }

    /// The gateway is validated and stored the way it will be used; `default` clears it.
    #[test]
    fn the_ipfs_gateway_is_validated_and_normalised() {
        let mut c = AppConfig::default();
        c.set("ipfs_gateway", "http://localhost:8080/ipfs/").unwrap();
        assert_eq!(c.ipfs_gateway.as_deref(), Some("http://127.0.0.1:8080"));
        assert!(c.set("ipfs_gateway", "http://public.example").is_err(), "public gateways need https");
        assert_eq!(c.ipfs_gateway.as_deref(), Some("http://127.0.0.1:8080"), "a refused value changes nothing");
        c.set("ipfs_gateway", "https://ipfs.qu.ai").unwrap();
        assert_eq!(c.ipfs_gateway, None, "the default is not stored");
        c.set("ipfs_gateway", "https://{cid}.ipfs.dweb.link").unwrap();
        let back: AppConfig = toml::from_str(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert_eq!(back.ipfs_gateway.as_deref(), Some("https://{cid}.ipfs.dweb.link"));
        c.set("ipfs_gateway", "default").unwrap();
        assert_eq!(c.ipfs_gateway, None);
    }

    /// Messaging starts off, trading and NFTs on, for a new install and for a config written
    /// before the switches existed; `config set` turns each one either way.
    #[test]
    fn features_default_to_messaging_off_and_can_be_switched() {
        let c = AppConfig::default();
        assert_eq!((c.features.messaging, c.features.trading, c.features.nfts), (false, true, true));
        let old: AppConfig = toml::from_str("default_network = \"mainnet\"\n").unwrap();
        assert_eq!(old.features, Features::default(), "an older config gets the defaults");
        let mut c = c;
        c.set("features.messaging", "on").unwrap();
        c.set("features.nfts", "off").unwrap();
        assert!(c.set("features.chess", "on").is_err());
        let back: AppConfig = toml::from_str(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert!(back.features.messaging && back.features.trading && !back.features.nfts);
        assert!(back.features.require(Feature::Nfts).unwrap_err().to_string().contains("features.nfts on"));
    }

    /// A new install starts Simple; a configuration written before the choice existed keeps
    /// showing everything; `config set mode` goes either way and survives a save.
    #[test]
    fn new_configs_are_simple_and_older_ones_stay_pro() {
        assert_eq!(AppConfig::default().mode, Mode::Simple);
        let old: AppConfig = toml::from_str("default_network = \"mainnet\"\nauto_lock_minutes = 5\n").unwrap();
        assert_eq!(old.mode, Mode::Pro, "an existing config keeps its full view");
        let mut c = AppConfig::default();
        let saved: AppConfig = toml::from_str(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert_eq!(saved.mode, Mode::Simple, "a new config says so once written");
        c.set("mode", "pro").unwrap();
        assert!(c.set("mode", "expert").is_err());
        let back: AppConfig = toml::from_str(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert_eq!(back.mode, Mode::Pro);
    }
}
