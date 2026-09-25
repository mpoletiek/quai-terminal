//! Guard rails for the overhaul (docs/ARCHITECTURE_REVIEW_2026-09-24.md).
//!
//! Two kinds of check, both over the source itself:
//!
//! - **Dependency direction.** Which workspace crate may depend on which. A crate that reaches
//!   past its layer (a feed parser into custody, say) fails here, not in review.
//! - **Ratchets.** Patterns the overhaul is removing. Each has a ceiling that only goes down: new
//!   code may not add one, and the phase that removes them sets the ceiling to zero. Test modules
//!   (`#[cfg(test)]` and `*_tests.rs`) are not counted.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).unwrap().to_path_buf()
}

fn members() -> BTreeMap<String, PathBuf> {
    let root: toml::Value = toml::from_str(&std::fs::read_to_string(workspace().join("Cargo.toml")).unwrap()).unwrap();
    let mut out = BTreeMap::new();
    for member in root["workspace"]["members"].as_array().unwrap() {
        let dir = workspace().join(member.as_str().unwrap());
        let manifest: toml::Value = toml::from_str(&std::fs::read_to_string(dir.join("Cargo.toml")).unwrap()).unwrap();
        out.insert(manifest["package"]["name"].as_str().unwrap().to_string(), dir);
    }
    out
}

/// Workspace crates a member depends on (normal dependencies, every target).
fn local_dependencies(dir: &Path, names: &BTreeSet<String>) -> BTreeSet<String> {
    let manifest: toml::Value = toml::from_str(&std::fs::read_to_string(dir.join("Cargo.toml")).unwrap()).unwrap();
    let mut tables = vec![manifest.get("dependencies").cloned()];
    if let Some(targets) = manifest.get("target").and_then(|t| t.as_table()) {
        for target in targets.values() {
            tables.push(target.get("dependencies").cloned());
        }
    }
    tables
        .into_iter()
        .flatten()
        .filter_map(|t| t.as_table().cloned())
        .flat_map(|t| t.keys().cloned().collect::<Vec<_>>())
        .filter(|k| names.contains(k))
        .collect()
}

/// The allowed edges. A crate missing from this table fails the test until its layer is decided.
const ALLOWED: &[(&str, &[&str])] = &[
    // Domain types: nothing of ours below them (docs/ARCHITECTURE_REVIEW_2026-09-24.md §5).
    ("quai-model", &[]),
    // Untrusted inputs (explorer, HTTP, IPFS, pictures): never the engine, never the vault.
    ("quai-feeds", &["quai-model"]),
    // The messaging protocol (board format, sealing, v3 wire): pure, no node and no store.
    ("quai-messaging", &["quai-model"]),
    // Venues: the tables of where a trade can happen and what it is pinned to. No node, no store.
    ("quai-venues", &[]),
    ("wallet-vault", &[]),
    ("wallet-core", &["quai-feeds", "quai-messaging", "quai-model", "quai-venues", "wallet-vault"]),
    // The engine runs wallets; it never touches the vault except through wallet-core's custody.
    ("quai-engine", &["wallet-core"]),
    ("quai-terminal-cli", &["quai-engine", "wallet-core", "wallet-vault"]),
];

#[test]
fn workspace_crates_depend_only_downward() {
    let members = members();
    let names: BTreeSet<String> = members.keys().cloned().collect();
    for (name, dir) in &members {
        let allowed: BTreeSet<String> = ALLOWED
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("{name} has no row in ALLOWED: decide its layer first"))
            .1
            .iter()
            .map(|s| s.to_string())
            .collect();
        let actual = local_dependencies(dir, &names);
        let extra: Vec<_> = actual.difference(&allowed).collect();
        assert!(extra.is_empty(), "{name} depends on {extra:?}, which its layer may not");
    }
}

/// Only the custody layer may see the vault: `registry` (vault lifecycle, and the one place its
/// errors are converted), `identity` (unsealed keys), `extras` (encrypted backups) and
/// `messaging/keys` (the messaging key file). `config` touches it without secrets: it borrows
/// the atomic private file write. `quai-model` never sees it (the dependency test says so).
#[test]
fn the_vault_is_reached_only_through_custody_modules() {
    let allowed = ["registry.rs", "identity.rs", "extras.rs", "messaging/keys.rs", "config.rs"];
    for file in source_files(&workspace().join("crates/wallet-core/src")) {
        let text = non_test_source(&file);
        if text.contains("wallet_vault::") {
            let rel = file.strip_prefix(workspace().join("crates/wallet-core/src")).unwrap().to_string_lossy().to_string();
            assert!(allowed.iter().any(|a| rel == *a), "{rel} reaches into the vault");
        }
    }
}

fn source_files(dir: &Path) -> Vec<PathBuf> {
    if dir.is_file() {
        return vec![dir.to_path_buf()];
    }
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") && !path.to_string_lossy().ends_with("_tests.rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// The file without its test module: everything before the first `#[cfg(test)]` line.
fn non_test_source(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap();
    match text.find("\n#[cfg(test)]") {
        Some(i) => text[..i].to_string(),
        None => text,
    }
}

fn count(dirs: &[&str], needles: &[&str]) -> usize {
    dirs.iter()
        .flat_map(|d| source_files(&workspace().join(d)))
        .map(|f| {
            let text = non_test_source(&f);
            needles.iter().map(|n| text.matches(n).count()).sum::<usize>()
        })
        .sum()
}

/// (what, directories, patterns, ceiling). Lower a ceiling when a phase removes some; never raise
/// one.
const RATCHETS: &[(&str, &[&str], &[&str], usize)] = &[
    // Phase 1 removed these: journal facts are reached through `journal::Detail`'s accessors and
    // kinds are `journal::OpKind`. The ceiling is zero; the compiler now refuses most of them.
    ("untyped journal detail reads", &["crates/wallet-core/src", "crates/quai-terminal/src"], &["detail[\"", "detail.get(\""], 0),
    (
        "operation kinds compared as text",
        &["crates/wallet-core/src", "crates/quai-terminal/src"],
        &["kind == \"", "kind != \"", "kind.as_str() ==", "OpKind::parse(\""],
        0,
    ),
    // Phase 3: every card, form and sequence acts from `Dashboard::active_account()`.
    ("the TUI acting from the first account", &["crates/quai-terminal/src/tui"], &["dash.accounts.first()"], 0),
    // Phase 6: remote values are `quai_engine::resource::Resource`s, whose freshness is one table
    // (`resource::fresh`); the UI keeps no clocks of its own for them.
    (
        "clocks for remote data kept by the UI",
        &["crates/quai-terminal/src/tui/eco"],
        &[
            "_at: Option<Instant>",
            "_at: Option<std::time::Instant>",
            "_at: HashMap<",
            "_asked: Option<Instant>",
            "_attempted: Option<Instant>",
        ],
        0,
    ),
    // Phase 6: files and databases are read and written off the UI thread (the persistence lane,
    // the data worker, the engine). Phase 7 took the last two, with the TUI's own trade flow.
    (
        "file or database I/O on the UI thread",
        &[
            "crates/quai-terminal/src/tui/eco",
            "crates/quai-terminal/src/tui/app",
            "crates/quai-terminal/src/tui/palette.rs",
            "crates/quai-terminal/src/tui/order_ui.rs",
        ],
        &[
            "AppDb::open(",
            "Session::open(",
            "DataCtx::open(",
            "std::fs::write(",
            "std::fs::read_to_string(",
            "load_summary(",
            "save_summary(",
        ],
        0,
    ),
    // Phase 9: what a screen is and does lives in its `tui::screen::ScreenView` impl; code that
    // still asks which screen is open (Enter's action, what is selected) stays at this count.
    (
        "dispatch on which screen is open, outside the screen impls",
        &["crates/quai-terminal/src/tui"],
        &["match self.nav.screen", "match app.nav.screen", "match (self.nav.screen", "match (app.nav.screen"],
        12,
    ),
    // Phase 7: multi-step trades are walked by the engine's one runner (`quai_engine::plans`);
    // the TUI starts them and follows them, and keeps no trade machine or checkpoint of its own.
    (
        "a trade machine in the TUI",
        &["crates/quai-terminal/src/tui"],
        &["FlowKind", "Coordinator::", "plans::claim(", "save_trade_plan(", "advance_allocation("],
        0,
    ),
    // Phase 5: the engine checks passwords and holds keys (in the daemon, or in a standalone
    // host); the TUI hands a password over and never opens a vault or holds keys itself.
    (
        "the TUI unlocking or holding keys",
        &["crates/quai-terminal/src/tui"],
        &["registry.unlock(", ".unlock_current(", "Cmd::UseKeys", ".use_keys(", "identity::Unlocked", "custody::"],
        0,
    ),
];

/// The named fields of a struct, from its source.
fn struct_fields(file: &str, name: &str) -> Vec<String> {
    let text = std::fs::read_to_string(workspace().join(file)).unwrap();
    let start = text.find(&format!("pub struct {name} {{")).unwrap_or_else(|| panic!("{name} in {file}"));
    let mut depth = 0;
    let mut end = start;
    for (i, c) in text[start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = start + i;
                    break;
                }
            }
            _ => {}
        }
    }
    text[start..end]
        .lines()
        .filter_map(|l| {
            let l = l.strip_prefix("    ")?;
            if l.starts_with(' ') || l.starts_with("//") || l.starts_with('#') {
                return None;
            }
            let l = l.strip_prefix("pub(crate) ").or_else(|| l.strip_prefix("pub ")).unwrap_or(l);
            let (name, _) = l.split_once(':')?;
            name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_').then(|| name.to_string())
        })
        .collect()
}

/// Phase 6: the UI's state is view state, grouped by what it is for; remote values are
/// resources. `App` and `Eco` together stay under 60 fields (they were 211).
#[test]
fn the_ui_state_stays_small() {
    let app = struct_fields("crates/quai-terminal/src/tui/app/mod.rs", "App");
    let eco = struct_fields("crates/quai-terminal/src/tui/eco/mod.rs", "Eco");
    assert!(app.len() > 10 && eco.len() > 5, "the field reader found nothing: {app:?} {eco:?}");
    let total = app.len() + eco.len();
    eprintln!("App {} + Eco {} = {total} fields", app.len(), eco.len());
    assert!(total < 60, "App {} + Eco {} = {total} fields: group new state where it belongs", app.len(), eco.len());
}

#[test]
fn ratchets_only_go_down() {
    let mut report = Vec::new();
    for (what, dirs, needles, ceiling) in RATCHETS {
        let now = count(dirs, needles);
        report.push(format!("{what}: {now} (ceiling {ceiling})"));
        assert!(now <= *ceiling, "{what}: {now} now, above its ceiling of {ceiling}. The overhaul is removing these; do not add more.");
    }
    eprintln!("{}", report.join("\n"));
}

/// Phase 7: the exchanges are rows in one table (`quai_venues::table::AMMS`). Nothing outside the
/// venues crate names a particular exchange beyond the main one, so adding a UniswapV2 exchange is
/// a variant, a row and its pins, all in `quai-venues`.
#[test]
fn exchanges_are_named_only_in_the_venue_table() {
    let mut found = Vec::new();
    for dir in [
        "crates/quai-model/src",
        "crates/quai-feeds/src",
        "crates/quai-messaging/src",
        "crates/wallet-core/src",
        "crates/quai-engine/src",
        "crates/quai-terminal/src",
    ] {
        for file in source_files(&workspace().join(dir)) {
            let text = non_test_source(&file);
            for name in ["Venue::LaunchAmm", "Venue::Legacy", "Venue::HartiiAmm"] {
                let n = text.matches(name).count();
                if n > 0 {
                    found.push(format!("{}: {name} ×{n}", file.display()));
                }
            }
        }
    }
    assert!(found.is_empty(), "exchange-specific code outside quai-venues:\n{}", found.join("\n"));
}
