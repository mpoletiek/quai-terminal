//! Actions: the palette, settings and data sources, networks, and saving preferences.

use super::*;

impl App {
    pub fn open_palette(&mut self) {
        self.load_palette_recent();
        self.modal = Modal::Palette { query: String::new(), selected: 0 };
    }

    pub fn run_action(&mut self, id: &str) {
        if let Some(feature) = action_feature(id).filter(|f| !self.config.features.on(*f)) {
            self.info(format!("{} · System › Settings", feature.off_note()));
            return;
        }
        let needs_keys = matches!(
            id,
            "send_quai"
                | "send_qi"
                | "send_token"
                | "convert_quai_qi"
                | "convert_qi_quai"
                | "wrap_qi"
                | "claim_wqi"
                | "unwrap_wqi"
                | "wrap_quai"
                | "unwrap_quai"
                | "approve"
                | "aggregate"
                | "discover"
                | "add_peer"
                | "notify"
                | "speedup"
                | "export_phrase"
                | "import_key"
                | "fill_gap"
        );
        if needs_keys && !self.can_sign() {
            self.toast("this wallet is watch-only", true);
            return;
        }
        match id {
            "send_quai" => self.open_form(FormKind::SendQuai),
            "send_qi" => self.open_form(FormKind::SendQi),
            "send_token" => self.open_form(FormKind::SendToken),
            "receive_quai" => self.modal = Modal::Receive { asset_qi: false, account: 0 },
            "receive_qi" => self.modal = Modal::Receive { asset_qi: true, account: 0 },
            "convert_quai_qi" => self.open_form(FormKind::ConvertQuaiToQi),
            "convert_qi_quai" => self.open_form(FormKind::ConvertQiToQuai),
            "quote" => self.open_form(FormKind::Quote),
            "wrap_qi" => self.open_form(FormKind::WrapQi),
            "claim_wqi" => self.claim_now(None),
            "unwrap_wqi" => self.open_form(FormKind::UnwrapWqi),
            "wrap_quai" => self.open_form(FormKind::WrapQuai),
            "unwrap_quai" => self.open_form(FormKind::UnwrapQuai),
            "approve" => self.open_form(FormKind::Approve),
            "import_token" => self.open_form(FormKind::ImportToken),
            // A watch-only wallet has nothing to derive from: its "add" is another address to watch.
            "add_account" if !self.can_sign() => self.open_form(FormKind::WatchAddress),
            "add_account" => self.open_form(FormKind::AddAccount),
            "import_key" => self.open_form(FormKind::ImportKey),
            "watch_address" if self.can_sign() => {
                self.info("this wallet holds keys, so it watches nothing; a new watch-only wallet follows an address");
                self.begin_onboarding(OnboardKind::Watch);
            }
            "watch_address" => self.open_form(FormKind::WatchAddress),
            "new_watch_wallet" => self.begin_onboarding(OnboardKind::Watch),
            "new_qi_address" => self.open_form(FormKind::NewQiAddress),
            "scan_qi" => self.send(Cmd::ScanQi { deep: None }),
            "deep_scan" => self.open_form(FormKind::DeepScan),
            "aggregate" => self.send(Cmd::Prepare(Prepare::Consolidate { aggregate: true })),
            "sweep" => self.send(Cmd::Prepare(Prepare::Consolidate { aggregate: false })),
            "discover" => self.send(Cmd::DiscoverMailbox),
            "add_peer" => {
                self.open_form(FormKind::Contact(None));
                if let Modal::Form(f) = &mut self.modal {
                    f.focus = 2;
                }
            }
            "notify" => self.open_form(FormKind::Notify),
            "add_contact" => self.open_form(FormKind::Contact(None)),
            "contacts" => self.switch(Screen::Contacts),
            "trade" => self.open_trade(),
            "swap" => self.switch(Screen::Swap),
            "portfolio" => self.switch(Screen::Home),
            "home" => self.switch(Screen::Home),
            "nfts" => self.switch(Screen::Collected),
            "explore" => self.switch(Screen::Explore),
            "listings" => self.switch(Screen::Listings),
            "data_sources" => self.switch(Screen::DataSources),
            "launches" => self.switch(Screen::Launches),
            "pnl" => self.switch(Screen::Pnl),
            "locks" => self.switch(Screen::Accounts),
            "discover_tokens" => self.send(Cmd::DiscoverTokens),
            "test_data" => self.send_data(super::super::data::DataCmd::Test),
            "speedup" => {
                let rows = self.activity_rows();
                match rows.get(self.nav.selected) {
                    Some((_, true, i)) if self.nav.screen == Screen::Activity && self.dash.ops[*i].status.replaceable() => {
                        let id = self.dash.ops[*i].id.clone();
                        self.send(Cmd::Prepare(Prepare::SpeedUp { op: id }));
                    }
                    Some((_, true, i)) if self.nav.screen == Screen::Activity && !self.dash.ops[*i].status.is_terminal() => {
                        self.toast("this transaction is already mined; nothing to speed up", true)
                    }
                    _ => self.toast("select a pending transaction on the activity screen first", true),
                }
            }
            "fill_gap" => self.send(Cmd::Prepare(Prepare::FillGap { from: None })),
            "themes" => self.modal = Modal::Themes(Picker::new(self)),
            // The terminal's own selection back, until the mouse is taken again (this action
            // again, or the setting). Shift-drag selects in most terminals without this.
            "mouse_release" => {
                self.input.mouse_released = !self.input.mouse_released;
                self.toast(
                    if self.input.mouse_released {
                        "mouse released · drag to select text · run this again to take it back"
                    } else {
                        "mouse on"
                    },
                    false,
                );
            }
            "glossary" => self.modal = Modal::Glossary { selected: 0 },
            "daemon_unlock" => match crate::daemon::state(&self.paths) {
                _ if !self.can_sign() => self.toast("this wallet is watch-only: the daemon watches it without keys", true),
                None => self.toast("the daemon is not running · quai-terminal daemon start", true),
                Some(d) if self.meta.as_ref().is_some_and(|m| d.unlocked(&m.id)) => {
                    self.info("the daemon already holds this wallet unlocked")
                }
                Some(_) => self.open_form(FormKind::DaemonUnlock),
            },
            "lock_gallery" => self.modal = Modal::Effects(Gallery::new(&self.config.lock_effect)),
            "refresh" => self.send(Cmd::Refresh { full: true }),
            "lock" => self.lock_now(None),
            "switch_account" => self.open_account_picker(),
            "export_phrase" => self.open_form(FormKind::ExportPhrase),
            "backup" => self.open_form(FormKind::Backup),
            "network" => self.switch(Screen::Network),
            "notifications" => {
                self.modal = Modal::Notifications;
                self.send(Cmd::MarkRead);
            }
            "matrix" => {
                if self.motion() == Motion::Off {
                    self.info("motion is off (Settings → Motion)");
                } else {
                    let args = super::super::fx::theme_args("matrix", &self.theme);
                    self.fx.ambient = Ceremony::with_args("matrix", &args, "follow the white rabbit", 100, 30, 420)
                        .map(|c| c.at_speed(super::super::fx::LOCK_SPEED));
                }
            }
            "poem" => {
                if self.motion() == Motion::Off {
                    self.info("motion is off (Settings → Motion)");
                } else {
                    match super::super::fx::poem_rain(self.fx.recent_hashes.iter().map(String::as_str)) {
                        Some(text) => {
                            let args = super::super::fx::theme_args("rain", &self.theme);
                            self.fx.ambient =
                                Ceremony::with_args("rain", &args, &text, 100, 30, 360).map(|c| c.at_speed(super::super::fx::LOCK_SPEED));
                            self.fx.poem_haiku = Some(super::super::fx::POEM_HAIKU.to_string());
                        }
                        None => self.info("waiting for a few blocks to fall"),
                    }
                }
            }
            "help" => self.modal = Modal::Help,
            "quit" => self.quit = true,
            _ => {}
        }
        self.dirty = true;
    }

    /// The settings this wallet has a use for: a watch-only wallet has no keys to lock, reveal,
    /// back up or hand to the daemon.
    pub fn settings_rows(&self) -> Vec<(&'static str, &'static str)> {
        let watch = self.meta.as_ref().is_some_and(|m| m.kind == wallet_core::registry::WalletKind::Watch);
        SETTINGS
            .iter()
            .copied()
            .filter(|(id, _)| !(watch && matches!(*id, "autolock" | "hold_to_sign" | "phrase" | "backup" | "daemon_unlock")))
            .collect()
    }

    /// Enter on a setting: open it, or step it forward.
    pub(crate) fn settings_action(&mut self) {
        self.settings_step(1);
    }

    /// ← on a setting: step it back (a switch just flips).
    pub(crate) fn settings_prev(&mut self) {
        self.settings_step(-1);
    }

    /// → on a setting: step it forward.
    pub(crate) fn settings_next(&mut self) {
        self.settings_step(1);
    }

    fn settings_step(&mut self, dir: i32) {
        fn cycle<T: PartialEq + Copy>(list: &[T], now: T, dir: i32) -> T {
            let i = list.iter().position(|v| *v == now).unwrap_or(0) as i32;
            list[(i + dir).rem_euclid(list.len() as i32) as usize]
        }
        let on_off = |b: bool| if b { "on" } else { "off" };
        let changed: Option<(String, String)> = match self.settings_rows().get(self.nav.selected).map(|s| s.0) {
            Some("theme") => {
                self.run_action("themes");
                None
            }
            Some("lock_effect") => {
                self.run_action("lock_gallery");
                None
            }
            Some("motion") => {
                self.config.motion = cycle(&[Motion::Vivid, Motion::Full, Motion::Reduced, Motion::Off], self.config.motion, dir);
                Some(("Motion".into(), format!("{:?}", self.config.motion).to_lowercase()))
            }
            Some("mouse") => {
                use wallet_core::config::MouseMode;
                self.config.mouse = cycle(&[MouseMode::Auto, MouseMode::Full, MouseMode::Click, MouseMode::Off], self.config.mouse, dir);
                self.input.mouse_released = false;
                Some(("Mouse".into(), format!("{:?}", self.config.mouse).to_lowercase()))
            }
            Some("lock_loop") => {
                self.config.lock_loop = !self.config.lock_loop;
                Some((
                    "Loop the lock screen animation".into(),
                    if self.config.lock_loop { "on · one after another".into() } else { "off · one per lock, then still".into() },
                ))
            }
            Some("vim_keys") => {
                self.config.vim_keys = !self.config.vim_keys;
                Some((
                    "Move with h j k l".into(),
                    if self.config.vim_keys { "on · the arrows move too".into() } else { "off · arrows only".into() },
                ))
            }
            Some("icons") => {
                use wallet_core::config::IconMode;
                self.config.icons = cycle(&[IconMode::Auto, IconMode::Nerd, IconMode::Unicode, IconMode::Ascii], self.config.icons, dir);
                Some(("Icons".into(), format!("{:?}", self.config.icons).to_lowercase()))
            }
            Some("background") => {
                use wallet_core::config::BackgroundMode;
                self.config.background =
                    cycle(&[BackgroundMode::Auto, BackgroundMode::Terminal, BackgroundMode::Solid], self.config.background, dir);
                Some(("Background".into(), format!("{:?}", self.config.background).to_lowercase()))
            }
            Some("layout") => {
                self.config.layout = cycle(&["auto", "standard", "trader", "focus"], self.config.layout.as_str(), dir).into();
                Some(("Layout".into(), self.config.layout.clone()))
            }
            Some(id) if let Some(feature) = Feature::ALL.into_iter().find(|f| id.strip_prefix("feature:") == Some(f.key())) => {
                let on = !self.config.features.on(feature);
                self.config.features.set(feature, on);
                if on {
                    // Warm what it shows now, rather than on the first visit.
                    self.eco.preloaded = false;
                    self.preload();
                }
                Some((feature.title().into(), if on { "on".into() } else { "off · hidden, and the daemon stops watching it".into() }))
            }
            Some("daemon") => {
                self.config.daemon_autostart = !self.config.daemon_autostart;
                let on = self.config.daemon_autostart;
                if on && !crate::daemon::daemon_running(&self.paths) {
                    let _ = crate::daemon::ensure_current(&self.paths, 20);
                }
                Some((
                    "Background daemon".into(),
                    if on { "starts with the terminal".into() } else { "off · quai-terminal daemon start".into() },
                ))
            }
            Some("daemon_unlock") => {
                self.config.daemon_share_unlock = !self.config.daemon_share_unlock;
                let on = self.config.daemon_share_unlock;
                Some((
                    "Unlock the daemon too".into(),
                    if on { "on · from the next unlock".into() } else { "off · quai-terminal daemon unlock".into() },
                ))
            }
            Some("ceremonies") => {
                self.config.ceremonies = !self.config.ceremonies;
                Some(("Effects & celebrations".into(), on_off(self.config.ceremonies).into()))
            }
            Some("hold_to_sign") => {
                self.config.hold_to_sign = !self.config.hold_to_sign;
                Some((
                    "Hold enter to sign".into(),
                    if self.config.hold_to_sign {
                        "on · a review signs once enter is held until its bar fills".into()
                    } else {
                        "off".into()
                    },
                ))
            }
            Some("sound") => {
                self.config.sound = !self.config.sound;
                self.fx.bell = self.config.sound;
                Some(("Terminal bell".into(), on_off(self.config.sound).into()))
            }
            Some("big_numbers") => {
                self.config.big_numbers = !self.config.big_numbers;
                Some(("Big balance digits".into(), on_off(self.config.big_numbers).into()))
            }
            Some("balance_in_bar") => {
                self.config.balance_in_bar = !self.config.balance_in_bar;
                Some(("Balance in the top bar".into(), on_off(self.config.balance_in_bar).into()))
            }
            Some("notifications") => {
                self.config.notifications = !self.config.notifications;
                Some(("Notifications".into(), on_off(self.config.notifications).into()))
            }
            Some("autolock") => {
                self.config.auto_lock_minutes = cycle(&[0, 5, 10, 30, 60], self.config.auto_lock_minutes, dir);
                let v =
                    if self.config.auto_lock_minutes == 0 { "off".to_string() } else { format!("{} min", self.config.auto_lock_minutes) };
                Some(("Auto-lock".into(), v))
            }
            Some("images") => {
                self.config.images = !self.config.images;
                self.config.token_icons = self.config.images;
                self.data_policy_changed();
                Some(("Images".into(), on_off(self.config.images).into()))
            }
            Some("ipfs") => {
                self.open_form(FormKind::IpfsGateway(wallet_core::ipfs::Content::Media));
                None
            }
            Some("abi_ipfs") => {
                self.open_form(FormKind::IpfsGateway(wallet_core::ipfs::Content::Abi));
                None
            }
            Some("phrase") => {
                self.run_action("export_phrase");
                None
            }
            Some("backup") => {
                self.run_action("backup");
                None
            }
            Some("refresh") => {
                self.run_action("refresh");
                None
            }
            _ => None,
        };
        if let Some((name, value)) = changed {
            self.save_config();
            // Every change is announced, so an accidental key never goes unnoticed.
            self.toast(format!("{name} · {value}  (← → to change)"), false);
        }
    }

    pub(crate) fn data_source_action(&mut self) {
        let on_off = |b: bool| if b { "on" } else { "off" };
        let changed: Option<(String, String)> = match DATA_SOURCES.get(self.nav.selected).map(|s| s.0) {
            Some("explorer_lookups") => {
                self.config.explorer_lookups = !self.config.explorer_lookups;
                Some(("Explorer lookups".into(), on_off(self.config.explorer_lookups).into()))
            }
            Some("market_data") => {
                self.config.fetch_prices = !self.config.fetch_prices;
                Some(("Market data".into(), on_off(self.config.fetch_prices).into()))
            }
            Some("images") => {
                self.config.images = !self.config.images;
                Some(("NFT images".into(), on_off(self.config.images).into()))
            }
            Some("token_icons") => {
                self.config.token_icons = !self.config.token_icons;
                Some(("Token icons".into(), on_off(self.config.token_icons).into()))
            }
            Some("test") => {
                self.run_action("test_data");
                None
            }
            _ => None,
        };
        if let Some((name, value)) = changed {
            self.save_config();
            self.data_policy_changed();
            self.toast(format!("{name} · {value}"), false);
        }
    }

    /// Save the preferences: serialized here, written by the persistence lane (two fsyncs are
    /// not a keypress's business).
    pub fn save_config(&mut self) {
        match toml::to_string_pretty(&self.config) {
            Ok(text) => self.persist.write(self.paths.config_file(), text),
            Err(e) => self.toast(format!("could not save preferences: {e}"), true),
        }
    }

    /// Wait for saved preferences to reach the disk (on quit, and before anything reads the file).
    pub fn flush_config(&self) {
        self.persist.flush();
    }

    /// A preference write that failed: say so.
    pub fn poll_persist(&mut self) {
        use super::super::persist::Read;
        if let Some(e) = self.persist.error() {
            self.toast(e, true);
        }
        while let Some((what, texts)) = self.persist.answer() {
            self.dirty = true;
            match what {
                Read::PaletteRecent { wallet } if self.meta.as_ref().is_some_and(|m| m.id == wallet) => {
                    self.palette_recent = texts
                        .into_iter()
                        .flatten()
                        .flat_map(|s| s.lines().filter(|l| !l.is_empty()).map(str::to_string).collect::<Vec<_>>())
                        .take(super::super::palette::RECENTS)
                        .collect();
                }
                Read::Summaries { network, wallets } if network == self.network_id => {
                    self.cockpit.summaries =
                        wallets.into_iter().zip(texts).filter_map(|(w, text)| Some((w, serde_json::from_str(&text?).ok()?))).collect();
                }
                _ => {}
            }
        }
    }

    /// Switch the worker to another network; balances from the old one are cleared immediately.
    pub(crate) fn switch_network(&mut self, id: String) {
        let name = self.dash.networks.iter().find(|(n, _)| *n == id).map(|(_, name)| name.clone()).unwrap_or_else(|| id.clone());
        self.network_id = id.clone();
        self.config.default_network = id.clone();
        self.save_config();
        let networks = std::mem::take(&mut self.dash.networks);
        self.dash = Dashboard {
            meta: self.dash.meta.clone(),
            network_id: id.clone(),
            network_name: name.clone(),
            unlocked: self.dash.unlocked,
            networks,
            ..Dashboard::default()
        };
        self.status.busy = Some(format!("connecting to {name}…"));
        self.send(Cmd::SwitchNetwork(id.clone()));
        self.reset_eco_for_network();
        let _ = id;
    }

    /// Set (after verifying chain id and genesis) or clear (`url` empty) a monitoring endpoint.
    pub(crate) fn set_monitor(&mut self, network: &str, url: &str, pathing: bool) {
        if url.is_empty() {
            if self.config.monitor_endpoints.remove(network).is_some() {
                self.save_config();
                self.data_policy_changed();
            }
            self.info(format!("{network}: monitoring uses the main RPC"));
            return;
        }
        let profile = match self.config.network(network) {
            Ok(mut p) => {
                p.monitor = Some(wallet_core::network::MonitorEndpoint { rpc_url: url.to_string(), use_pathing: pathing });
                p
            }
            Err(e) => return self.toast(e.to_string(), true),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.tasks.monitor_check = Some(rx);
        self.status.busy = Some(format!("checking {url} against {network}…"));
        let network = network.to_string();
        std::thread::spawn(move || {
            let endpoint = profile.monitor.clone().expect("set above");
            let result = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string()).and_then(|rt| {
                rt.block_on(async {
                    let node = profile.monitor_node()?;
                    wallet_core::network::require_identity(&profile, &node.provider).await
                })
                .map_err(|e| e.to_string())
            });
            let _ = tx.send((network, endpoint, result));
        });
    }

    /// Test an IPFS gateway on its own thread, then save it if it answers for the content it is
    /// being set for (empty goes back to the built-in one, which needs no test).
    ///
    /// Saved when it serves the test file byte for byte, and also when it answers but cannot find
    /// the file in time — a node that has just started may need a while to reach the network, and
    /// that is not a reason to refuse it. Not saved when it cannot be reached at all, or when it
    /// returns content that does not match the CID it was asked for.
    pub(crate) fn set_ipfs_gateway(&mut self, content: wallet_core::ipfs::Content, url: &str) {
        // Empty means "back to the built-in gateway for this content", which needs no test.
        let url = url.trim();
        if url.is_empty() || url == "default" {
            match content {
                wallet_core::ipfs::Content::Abi => self.config.abi_ipfs_gateway = None,
                wallet_core::ipfs::Content::Media => self.config.ipfs_gateway = None,
            }
            let _ = wallet_core::ipfs::set_gateway(content, None);
            self.save_config();
            self.eco.media.images.retain(|_, slot| !matches!(slot, super::super::eco::ImageSlot::Failed(_)));
            return self.toast(format!("{} now uses {}", content.label(), content.default_gateway()), false);
        }
        let gateway = match wallet_core::ipfs::Gateway::parse(url) {
            Ok(g) => g,
            Err(e) => return self.toast(e.to_string(), true),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.tasks.ipfs_check = Some(rx);
        self.status.busy = Some(format!("testing {}…", gateway.display()));
        std::thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
                .and_then(|rt| rt.block_on(wallet_core::ipfs::test(&gateway)).map_err(|e| e.to_string()));
            let _ = tx.send((content, gateway, result));
        });
    }

    /// The wallet after the open one, in the order the Wallets screen lists them (wrapping).
    pub(crate) fn next_wallet_id(&mut self) -> Option<String> {
        self.load_wallets();
        let current = self.meta.as_ref().map(|m| m.id.clone());
        let i = self.cockpit.list.iter().position(|w| Some(&w.id) == current.as_ref())?;
        (self.cockpit.list.len() > 1).then(|| self.cockpit.list[(i + 1) % self.cockpit.list.len()].id.clone())
    }
}
