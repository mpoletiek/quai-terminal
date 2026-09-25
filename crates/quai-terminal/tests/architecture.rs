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
    tables.into_iter().flatten().filter_map(|t| t.as_table().cloned()).flat_map(|t| t.keys().cloned().collect::<Vec<_>>()).filter(|k| names.contains(k)).collect()
}

/// The allowed edges. A crate missing from this table fails the test until its layer is decided.
const ALLOWED: &[(&str, &[&str])] = &[
    ("wallet-vault", &[]),
    ("wallet-core", &["wallet-vault"]),
    ("quai-terminal-cli", &["wallet-core", "wallet-vault"]),
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

/// Only the custody layer may see the vault: `registry` (vault lifecycle), `identity` (unsealed
/// keys), `extras` (encrypted backups) and `messaging/keys` (the messaging key file). Two more
/// touch it without secrets: `error` maps its errors, and `config` borrows its atomic private
/// file write.
#[test]
fn the_vault_is_reached_only_through_custody_modules() {
    let allowed = ["registry.rs", "identity.rs", "extras.rs", "messaging/keys.rs", "error.rs", "config.rs"];
    for file in source_files(&workspace().join("crates/wallet-core/src")) {
        let text = non_test_source(&file);
        if text.contains("wallet_vault::") {
            let rel = file.strip_prefix(workspace().join("crates/wallet-core/src")).unwrap().to_string_lossy().to_string();
            assert!(allowed.iter().any(|a| rel == *a), "{rel} reaches into the vault");
        }
    }
}

fn source_files(dir: &Path) -> Vec<PathBuf> {
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
    ("untyped journal detail reads (phase 1)", &["crates/wallet-core/src"], &["detail[\""], 115),
    ("operation kinds compared as text (phase 1)", &["crates/wallet-core/src"], &["kind == \"", "kind != \""], 31),
    ("the TUI acting from the first account (phase 3)", &["crates/quai-terminal/src/tui"], &["accounts.first()"], 27),
];

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
