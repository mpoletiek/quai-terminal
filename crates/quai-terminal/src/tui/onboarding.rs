//! First-run onboarding: a welcome, then 1 look (theme showroom and motion) · 2 privacy ·
//! 3 connections · 4 wallet (create/import/watch) · 5 protect.

use super::app::{App, Field, OnboardKind, Onboarding, Picker, PickerOutcome};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use wallet_core::config::Motion;
use wallet_core::registry::WalletKind;
use zeroize::{Zeroize, Zeroizing};

pub const CHOICES: [(&str, &str); 4] = [
    ("Create a new wallet", "fresh 24-word recovery phrase · QUAI + Qi"),
    ("Import a recovery phrase", "from Pelagus or any BIP39 wallet"),
    ("Import a private key", "a single Quai account"),
    ("Watch addresses", "read-only · no keys on this computer"),
];

/// The motion choice, most to least. Each one previews as the cursor lands on it.
pub const MOTIONS: [(Motion, &str, &str); 4] = [
    (Motion::Vivid, "Vivid", "light turns along the edges while you work, and rests when you stop"),
    (Motion::Full, "Full", "screens slide and moments land · nothing moves on its own"),
    (Motion::Reduced, "Reduced", "only small, quick transitions"),
    (Motion::Off, "Off", "nothing moves"),
];

/// Above the privacy answers: what the question is about, and the part it is not about.
pub const PRIVACY_INTRO: &str = "This is only about the explorer, an indexer a third party runs. Balances, sends and reviews always come from a Quai node: your own if you set one next, otherwise rpc.quai.network, and that node sees the addresses it is asked about whichever you pick here.";

/// One answer to the privacy question: what it is, what it gets you, and what it costs.
pub struct PrivacyChoice {
    pub label: &'static str,
    pub says: &'static str,
    pub gets: &'static str,
    pub costs: &'static str,
    /// Address-linked explorer lookups (and the pictures and icons that come with them) on.
    pub connected: bool,
}

/// The privacy choice, private first: it is the default and it tells the explorer nothing. It is
/// only about the explorer; which node answers balances and reviews is the next step, and the
/// screen says so, because that is the part people take it to mean.
pub const PRIVACY: [PrivacyChoice; 2] = [
    PrivacyChoice {
        label: "Private",
        says: "explorer.qu.ai is never told your addresses",
        gets: "balances, your own transactions, tokens you add, prices and markets (no address sent)",
        costs: "tokens and NFTs sent to you stay hidden until you add them · no value history or pictures",
        connected: false,
    },
    PrivacyChoice {
        label: "Connected",
        says: "explorer.qu.ai looks up each of your addresses",
        gets: "tokens and NFTs sent to you, token transfers, value history, verified contracts, pictures",
        costs: "a third party learns your addresses and IP, and can link them · slow explorer, slow lists",
        connected: true,
    },
];

/// Where each kind of request goes with these settings: the "what goes where" the connections
/// step shows under its fields, so the node, the RPC and the explorer are not a guess.
/// `node` is the node URL as typed (empty: none).
pub fn routes(node: &str, connected: bool, rpc_url: &str) -> Vec<(&'static str, String, &'static str)> {
    let rpc = wallet_core::http::host_of(rpc_url).unwrap_or_else(|_| "the network's RPC".into());
    let node = node.trim();
    let reads = if node.is_empty() {
        (rpc.clone(), "balances, quotes, every review · sees each address asked about, and your IP")
    } else {
        (
            node.trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/').to_string(),
            "every read, reviews too · the RPC stands in if it stops answering · warns at 3+ blocks behind",
        )
    };
    vec![
        ("reads", reads.0, reads.1),
        ("sends", rpc, "every transaction goes here, whichever node reads · sees what you send, and your IP"),
        (
            "address lookups",
            if connected { "explorer.qu.ai".into() } else { "none".into() },
            if connected { "finds what was sent to you · sees your addresses and IP" } else { "the explorer is told nothing about you" },
        ),
        ("prices, markets", "explorer.qu.ai · graph.quai.network · hartiilabs.com".into(), "no address is ever sent"),
    ]
}

/// Step number shown in the onboarding header.
pub fn step(ob: &Onboarding) -> usize {
    match ob {
        Onboarding::Welcome => 0,
        Onboarding::Theme(_) | Onboarding::Motion { .. } => 1,
        Onboarding::Privacy { .. } => 2,
        Onboarding::Connections { .. } => 3,
        Onboarding::Choose { .. } => 4,
        _ => 5,
    }
}

/// The connections step, one line per field: what it is, and what you get for setting it. Shown
/// beside the field that has the cursor, so the reason is in front of the person deciding.
pub const CONNECTIONS: [(&str, &str); 3] = [
    (
        "Your own node",
        "A Quai node you run. Every read goes to it, reviews too, so it is what you take a review's numbers from, and nobody else learns the addresses you ask about. It must prove the same chain and genesis first, the public RPC stands in if it stops answering, and over the internet it must be https. Empty: read from the public RPC.",
    ),
    (
        "Contract details",
        "Where the wallet looks up what a contract is, so a review can say what a call will do instead of showing raw bytes. ipfs.qu.ai is where Quai's own tools publish these; change it only if you publish them yourself.",
    ),
    (
        "Pictures",
        "Where NFT pictures, token logos and their descriptions come from. This is the bulk of the fetching and the most revealing — point it at your own IPFS node and none of it leaves your network.",
    ),
];

/// The connections step's fields, filled in with whatever is already configured.
pub fn connection_fields(app: &App) -> Vec<Field> {
    let monitor = app.config.monitor_endpoints.get(&app.network_id).map(|m| m.rpc_url.clone()).unwrap_or_default();
    vec![
        Field::new("Your own node", "e.g. http://10.0.0.12:9200").with(monitor).optional(),
        Field::new("Contract details", wallet_core::ipfs::DEFAULT_ABI_GATEWAY)
            .with(app.config.abi_ipfs_gateway.clone().unwrap_or_default())
            .optional(),
        Field::new("Pictures", wallet_core::ipfs::DEFAULT_MEDIA_GATEWAY)
            .with(app.config.ipfs_gateway.clone().unwrap_or_default())
            .optional(),
    ]
}

/// Store what the connections step was given. Each value is checked for shape here; a monitoring
/// node is proven before it is ever read from (the session verifies its chain and genesis first),
/// and a gateway is tested the first time something is fetched through it.
pub fn apply_connections(app: &mut App, fields: &[Field]) -> Result<(), (usize, String)> {
    let value = |i: usize| fields.get(i).map(|f| f.value.trim().to_string()).unwrap_or_default();
    // Everything is checked before anything is written. Applying as it goes would leave the
    // earlier fields stored in memory when a later one is refused, and the next `save_config` from
    // anywhere would put a setting on disk that the user never got to confirm.
    let monitor = value(0);
    if !monitor.is_empty() && !(monitor.starts_with("http://") || monitor.starts_with("https://")) {
        return Err((0, "a node URL starts with http:// or https://".into()));
    }
    // Reviews read from this node, so a stranger on the path must not be able to answer for it.
    if !monitor.is_empty()
        && wallet_core::network::require_secure_rpc(&monitor, app.config.allow_insecure_rpc.contains(&app.network_id)).is_err()
    {
        return Err((0, "a node over the internet needs https; plain http is for this machine or your own network".into()));
    }
    let mut gateways = Vec::new();
    for (i, content) in [(1, wallet_core::ipfs::Content::Abi), (2, wallet_core::ipfs::Content::Media)] {
        let typed = value(i);
        let stored = if typed.is_empty() {
            None
        } else {
            let gateway = wallet_core::ipfs::Gateway::parse(&typed).map_err(|e| (i, e.to_string()))?;
            (!gateway.is_default_for(content)).then(|| gateway.display())
        };
        gateways.push((content, stored));
    }

    if monitor.is_empty() {
        app.config.monitor_endpoints.remove(&app.network_id);
    } else {
        app.config
            .monitor_endpoints
            .insert(app.network_id.clone(), wallet_core::network::MonitorEndpoint { rpc_url: monitor, use_pathing: true });
    }
    for (content, stored) in gateways {
        match content {
            wallet_core::ipfs::Content::Abi => app.config.abi_ipfs_gateway = stored.clone(),
            wallet_core::ipfs::Content::Media => app.config.ipfs_gateway = stored.clone(),
        }
        let _ = wallet_core::ipfs::set_gateway(content, stored.as_deref());
    }
    app.save_config();
    app.data_policy_changed();
    Ok(())
}

/// Apply a privacy choice to the preferences and save them.
pub fn apply_privacy(app: &mut App, connected: bool) {
    let c = &mut app.config;
    c.explorer_lookups = connected;
    c.images = connected;
    c.token_icons = connected;
    // The choice was made with the explanation in front of the user; no later notice is needed.
    c.data_disclosure_shown = true;
    app.save_config();
}

/// Start the flow for another wallet: creating one shows and verifies a fresh phrase first,
/// importing one asks for what is being imported. The same steps the first wallet used.
pub fn start(app: &mut crate::tui::app::App, kind: OnboardKind) -> Onboarding {
    match kind {
        OnboardKind::Create => match wallet_core::identity::generate_phrase(24, "english") {
            Ok(phrase) => Onboarding::ShowPhrase { phrase },
            Err(e) => {
                app.toast(e.to_string(), true);
                Onboarding::Choose { selected: 0 }
            }
        },
        other => details(other, None, true),
    }
}

fn details(kind: OnboardKind, phrase: Option<Zeroizing<String>>, verified: bool) -> Onboarding {
    let mut fields = vec![Field::new("Wallet name", "letters, digits, - and _").with("main")];
    match kind {
        OnboardKind::ImportPhrase => {
            fields.push(Field::new("Recovery phrase", "words separated by spaces · paste works").secret());
            fields.push(Field::new("BIP39 passphrase", "leave empty unless you set one").secret().optional());
        }
        OnboardKind::ImportKey => fields.push(Field::new("Private key", "hex, 0x optional · paste works").secret()),
        OnboardKind::Watch => fields.push(Field::new("Addresses", "Quai or Qi addresses separated by spaces")),
        OnboardKind::Create => {}
    }
    if kind != OnboardKind::Watch {
        fields.push(Field::new("Password", "encrypts the wallet on this computer").new_secret());
        fields.push(Field::new("Confirm password", "type it again").secret());
    }
    let focus = usize::from(kind != OnboardKind::Create && kind != OnboardKind::Watch);
    Onboarding::Details { kind, phrase, fields, focus, verified }
}

fn quiz_indexes(words: usize) -> [usize; 3] {
    let mut seed = [0u8; 8];
    let _ = wallet_core::sdk::crypto::fill_random(&mut seed);
    let mut n = u64::from_le_bytes(seed);
    let mut picks = Vec::new();
    while picks.len() < 3 {
        let i = (n % words as u64) as usize;
        n = n.rotate_left(17) ^ 0x9E37_79B9_7F4A_7C15;
        if !picks.contains(&i) {
            picks.push(i);
        }
    }
    picks.sort_unstable();
    [picks[0], picks[1], picks[2]]
}

fn next_network(app: &mut App) {
    let nets = app.config.networks();
    if let Some(i) = nets.iter().position(|n| n.id == app.network_id) {
        app.network_id = nets[(i + 1) % nets.len()].id.clone();
    } else if let Some(n) = nets.first() {
        app.network_id = n.id.clone();
    }
}

/// Where Esc goes from the first step of making a wallet: the choice of kind on first run, and
/// straight out when a wallet is already open (adding another from System › Wallets). That path
/// used to walk back into the first-run setup and loop there with no way out but finishing.
fn back_to_choose(app: &App) -> Option<Onboarding> {
    if app.meta.is_some() { None } else { Some(Onboarding::Choose { selected: 0 }) }
}

/// The motion step, with the cursor on what is set now.
fn motion_step(app: &App) -> Onboarding {
    let from = app.config.motion;
    Onboarding::Motion { selected: MOTIONS.iter().position(|m| m.0 == from).unwrap_or(0), from }
}

pub fn on_key(app: &mut App, key: KeyEvent) {
    if app.tasks.creating.is_some() {
        return;
    }
    let Some(state) = app.onboarding.take() else { return };
    app.dirty = true;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    app.onboarding = match state {
        Onboarding::Welcome => match key.code {
            KeyCode::Enter | KeyCode::Char(' ') => Some(Onboarding::Theme(Picker::new(app))),
            _ => Some(Onboarding::Welcome),
        },
        Onboarding::Theme(mut picker) => match picker.on_key(key, &mut app.theme) {
            PickerOutcome::Open => Some(Onboarding::Theme(picker)),
            PickerOutcome::Applied => {
                if let Some(e) = picker.current() {
                    app.config.theme = e.id.clone();
                    app.term.pending_theme_reload = true;
                    app.save_config();
                }
                Some(motion_step(app))
            }
            PickerOutcome::Cancelled => Some(motion_step(app)),
        },
        Onboarding::Motion { selected, from } => {
            let moved = match key.code {
                KeyCode::Up | KeyCode::Char('k') => Some(selected.saturating_sub(1)),
                KeyCode::Down | KeyCode::Char('j') => Some((selected + 1).min(MOTIONS.len() - 1)),
                _ => None,
            };
            match (moved, key.code) {
                (Some(selected), _) => {
                    // A preview only, written on enter: the frame draws itself in again, as a
                    // screen does, for the choices that animate, and Vivid turns its light.
                    app.config.motion = MOTIONS[selected].0;
                    app.fx.edge_intro = Some(std::time::Instant::now());
                    Some(Onboarding::Motion { selected, from })
                }
                (None, KeyCode::Enter) => {
                    app.config.motion = MOTIONS[selected].0;
                    app.save_config();
                    Some(Onboarding::Privacy { selected: 0 })
                }
                (None, KeyCode::Esc) => {
                    app.config.motion = from;
                    Some(Onboarding::Theme(Picker::new(app)))
                }
                _ => Some(Onboarding::Motion { selected, from }),
            }
        }
        Onboarding::Privacy { selected } => match key.code {
            KeyCode::Up | KeyCode::Char('k') => Some(Onboarding::Privacy { selected: selected.saturating_sub(1) }),
            KeyCode::Down | KeyCode::Char('j') => Some(Onboarding::Privacy { selected: (selected + 1).min(PRIVACY.len() - 1) }),
            KeyCode::Esc => Some(motion_step(app)),
            KeyCode::Enter => {
                apply_privacy(app, PRIVACY[selected].connected);
                Some(Onboarding::Connections { fields: connection_fields(app), focus: 0 })
            }
            _ => Some(Onboarding::Privacy { selected }),
        },
        Onboarding::Connections { mut fields, mut focus } => match key.code {
            KeyCode::Esc => Some(Onboarding::Privacy { selected: usize::from(app.config.explorer_lookups) }),
            KeyCode::Tab | KeyCode::Down => {
                focus = (focus + 1) % fields.len();
                Some(Onboarding::Connections { fields, focus })
            }
            KeyCode::BackTab | KeyCode::Up => {
                focus = (focus + fields.len() - 1) % fields.len();
                Some(Onboarding::Connections { fields, focus })
            }
            KeyCode::Backspace => {
                fields[focus].value.pop();
                Some(Onboarding::Connections { fields, focus })
            }
            KeyCode::Char('u') if ctrl => {
                fields[focus].value.clear();
                Some(Onboarding::Connections { fields, focus })
            }
            // Enter walks the fields, then saves. Everything here has a working default, so
            // holding enter goes straight through without deciding anything.
            KeyCode::Enter if focus + 1 < fields.len() => Some(Onboarding::Connections { fields, focus: focus + 1 }),
            KeyCode::Enter => match apply_connections(app, &fields) {
                Ok(()) => Some(Onboarding::Choose { selected: 0 }),
                Err((i, e)) => {
                    app.toast(e, true);
                    Some(Onboarding::Connections { fields, focus: i })
                }
            },
            KeyCode::Char(c) if !ctrl => {
                if fields[focus].value.chars().count() < 256 {
                    fields[focus].value.push(c);
                }
                Some(Onboarding::Connections { fields, focus })
            }
            _ => Some(Onboarding::Connections { fields, focus }),
        },
        Onboarding::Choose { selected } => match key.code {
            KeyCode::Up | KeyCode::Char('k') => Some(Onboarding::Choose { selected: selected.saturating_sub(1) }),
            KeyCode::Down | KeyCode::Char('j') => Some(Onboarding::Choose { selected: (selected + 1).min(CHOICES.len() - 1) }),
            KeyCode::Char('n') => {
                next_network(app);
                Some(Onboarding::Choose { selected })
            }
            KeyCode::Char('t') => Some(Onboarding::Theme(Picker::new(app))),
            // First run walks back through setup; adding a wallet from System › Wallets leaves.
            KeyCode::Esc if app.meta.is_some() => None,
            KeyCode::Esc => Some(Onboarding::Connections { fields: connection_fields(app), focus: 0 }),
            KeyCode::Enter => match selected {
                0 => match wallet_core::identity::generate_phrase(24, "english") {
                    Ok(phrase) => Some(Onboarding::ShowPhrase { phrase }),
                    Err(e) => {
                        app.toast(e.to_string(), true);
                        Some(Onboarding::Choose { selected })
                    }
                },
                1 => Some(details(OnboardKind::ImportPhrase, None, true)),
                2 => Some(details(OnboardKind::ImportKey, None, true)),
                _ => Some(details(OnboardKind::Watch, None, true)),
            },
            _ => Some(Onboarding::Choose { selected }),
        },
        Onboarding::ShowPhrase { phrase } => match key.code {
            KeyCode::Enter => {
                let n = phrase.split_whitespace().count();
                Some(Onboarding::Quiz { indexes: quiz_indexes(n), phrase, answers: Default::default(), focus: 0 })
            }
            // Leaving discards this phrase; nothing was saved yet.
            KeyCode::Esc => back_to_choose(app),
            _ => Some(Onboarding::ShowPhrase { phrase }),
        },
        Onboarding::Quiz { phrase, indexes, mut answers, mut focus } => match key.code {
            KeyCode::Esc => {
                for a in &mut answers {
                    a.zeroize();
                }
                Some(Onboarding::ShowPhrase { phrase })
            }
            KeyCode::Char('s') if ctrl => Some(details(OnboardKind::Create, Some(phrase), false)),
            KeyCode::Tab | KeyCode::Down => {
                focus = (focus + 1) % 3;
                Some(Onboarding::Quiz { phrase, indexes, answers, focus })
            }
            KeyCode::BackTab | KeyCode::Up => {
                focus = (focus + 2) % 3;
                Some(Onboarding::Quiz { phrase, indexes, answers, focus })
            }
            KeyCode::Backspace => {
                answers[focus].pop();
                Some(Onboarding::Quiz { phrase, indexes, answers, focus })
            }
            KeyCode::Enter if focus < 2 => Some(Onboarding::Quiz { phrase, indexes, answers, focus: focus + 1 }),
            KeyCode::Enter => {
                let words: Vec<&str> = phrase.split_whitespace().collect();
                let ok = (0..3).all(|i| answers[i].trim().eq_ignore_ascii_case(words[indexes[i]]));
                if ok {
                    Some(details(OnboardKind::Create, Some(phrase), true))
                } else {
                    // Keep correct answers; clear and name only the wrong ones.
                    let wrong: Vec<usize> = (0..3).filter(|&i| !answers[i].trim().eq_ignore_ascii_case(words[indexes[i]])).collect();
                    let names: Vec<String> = wrong.iter().map(|&i| format!("#{}", indexes[i] + 1)).collect();
                    app.toast(format!("word {} doesn't match — check your written copy", names.join(" and ")), true);
                    for &i in &wrong {
                        answers[i].zeroize();
                    }
                    let focus = wrong.first().copied().unwrap_or(0);
                    Some(Onboarding::Quiz { phrase, indexes, answers, focus })
                }
            }
            KeyCode::Char(c) if !ctrl => {
                answers[focus].push(c);
                Some(Onboarding::Quiz { phrase, indexes, answers, focus })
            }
            _ => Some(Onboarding::Quiz { phrase, indexes, answers, focus }),
        },
        Onboarding::Details { kind, phrase, mut fields, mut focus, verified } => match key.code {
            KeyCode::Esc => {
                for f in &mut fields {
                    f.value.zeroize();
                }
                match (kind, phrase) {
                    (OnboardKind::Create, Some(phrase)) => Some(Onboarding::ShowPhrase { phrase }),
                    _ => back_to_choose(app),
                }
            }
            KeyCode::Tab | KeyCode::Down => {
                focus = (focus + 1) % fields.len();
                Some(Onboarding::Details { kind, phrase, fields, focus, verified })
            }
            KeyCode::BackTab | KeyCode::Up => {
                focus = (focus + fields.len() - 1) % fields.len();
                Some(Onboarding::Details { kind, phrase, fields, focus, verified })
            }
            KeyCode::Backspace => {
                fields[focus].value.pop();
                Some(Onboarding::Details { kind, phrase, fields, focus, verified })
            }
            KeyCode::Char('u') if ctrl => {
                fields[focus].value.zeroize();
                Some(Onboarding::Details { kind, phrase, fields, focus, verified })
            }
            KeyCode::Enter if focus + 1 < fields.len() => Some(Onboarding::Details { kind, phrase, fields, focus: focus + 1, verified }),
            KeyCode::Enter => match start_create(app, &kind, phrase.as_deref().map(|s| s.as_str()), &fields, verified) {
                Ok(()) => Some(Onboarding::Details { kind, phrase, fields, focus, verified }),
                Err((i, e)) => {
                    app.toast(e, true);
                    Some(Onboarding::Details { kind, phrase, fields, focus: i.unwrap_or(focus), verified })
                }
            },
            KeyCode::Char(c) if !ctrl => {
                if fields[focus].value.chars().count() < 1024 {
                    fields[focus].value.push(c);
                }
                Some(Onboarding::Details { kind, phrase, fields, focus, verified })
            }
            _ => Some(Onboarding::Details { kind, phrase, fields, focus, verified }),
        },
    };
}

/// Validate, then derive keys and seal the vault on a background thread.
fn start_create(
    app: &mut App,
    kind: &OnboardKind,
    generated: Option<&str>,
    fields: &[Field],
    verified: bool,
) -> Result<(), (Option<usize>, String)> {
    let pos = |label: &str| fields.iter().position(|f| f.label == label);
    let get = |label: &str| fields.iter().find(|f| f.label == label).map(|f| f.value.trim().to_string()).unwrap_or_default();
    let name = get("Wallet name");
    if name.is_empty() {
        return Err((Some(0), "give the wallet a name".into()));
    }
    let password = Zeroizing::new(fields.iter().find(|f| f.label == "Password").map(|f| f.value.clone()).unwrap_or_default());
    if *kind != OnboardKind::Watch {
        if password.chars().count() < wallet_vault::MIN_PASSWORD_CHARS {
            return Err((pos("Password"), format!("password must be at least {} characters", wallet_vault::MIN_PASSWORD_CHARS)));
        }
        if *password != fields.iter().find(|f| f.label == "Confirm password").map(|f| f.value.clone()).unwrap_or_default() {
            return Err((pos("Confirm password"), "passwords do not match".into()));
        }
    }
    let secret = Zeroizing::new(match kind {
        OnboardKind::Create => generated.unwrap_or_default().to_string(),
        OnboardKind::ImportPhrase => {
            let phrase = get("Recovery phrase");
            wallet_core::identity::parse_mnemonic(&phrase, "english").map_err(|e| (pos("Recovery phrase"), e.to_string()))?;
            phrase
        }
        OnboardKind::ImportKey => get("Private key"),
        OnboardKind::Watch => get("Addresses"),
    });
    if *kind == OnboardKind::Watch && secret.split_whitespace().next().is_none() {
        return Err((pos("Addresses"), "enter at least one address".into()));
    }
    let passphrase = Zeroizing::new(get("BIP39 passphrase"));
    let registry = app.registry.clone();
    let kind = kind.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = match kind {
            OnboardKind::Create => registry.create_hd(&name, &secret, "english", "", &password, verified),
            OnboardKind::ImportPhrase => registry.create_hd(&name, &secret, "english", &passphrase, &password, true),
            OnboardKind::ImportKey => registry.create_from_key(&name, &secret, &password),
            OnboardKind::Watch => {
                let pairs: Vec<(String, String)> =
                    secret.split_whitespace().enumerate().map(|(i, a)| (a.to_string(), format!("Watch {}", i + 1))).collect();
                registry.create_watch(&name, &pairs)
            }
        };
        let _ = tx.send(result.map_err(|e| e.to_string()).map(|meta| {
            let unlock = (meta.kind != WalletKind::Watch).then(|| password.clone());
            (meta, unlock)
        }));
    });
    app.tasks.creating = Some(rx);
    let sealing = fields.iter().any(|f| f.label == "Password");
    app.status.busy = Some(if sealing { "deriving keys and encrypting your vault…".into() } else { "saving…".into() });
    Ok(())
}
