//! Record the `quai-sdk` version this binary is built against, read from the workspace lockfile,
//! so `diagnostics info` reports the SDK actually linked rather than a string someone must
//! remember to update (it said alpha.1 through alpha.9).

fn main() {
    let lock = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock.display());
    let version = std::fs::read_to_string(&lock)
        .ok()
        .and_then(|text| {
            let mut lines = text.lines();
            while let Some(line) = lines.next() {
                if line.trim() == "name = \"quai-sdk\"" {
                    return lines.next()?.trim().strip_prefix("version = \"")?.strip_suffix('"').map(str::to_string);
                }
            }
            None
        })
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=QUAI_SDK_VERSION={version}");
}
