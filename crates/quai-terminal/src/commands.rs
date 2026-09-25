//! Command handlers.

use crate::args::*;
use crate::output::{Out, qr_text};
use crate::prompt;
use serde_json::json;
use wallet_core::amount;
use wallet_core::appdb::OpStatus;
use wallet_core::config::AppConfig;
use wallet_core::extras;
use wallet_core::network::{self, NetworkProfile};
use wallet_core::paths::Paths;
use wallet_core::registry::{Registry, WalletKind, WalletMeta, now};
use wallet_core::sdk::U256;
use wallet_core::session::{Session, short_address};
use wallet_core::track::{describe, human_duration};
use wallet_core::tx::{Review, Submitted};
use wallet_core::{CoreError, Result};

/// Shared command context.
pub struct Ctx {
    pub paths: Paths,
    pub config: AppConfig,
    pub registry: Registry,
    pub out: Out,
    pub global: Global,
}

impl Ctx {
    pub fn new(global: Global) -> Result<Self> {
        let paths = Paths::resolve(global.home.clone())?;
        let config = AppConfig::load(&paths)?;
        let registry = Registry::new(paths.clone());
        let color = !global.no_color && global.output == Output::Human && std::io::IsTerminal::is_terminal(&std::io::stdout());
        Ok(Self { out: Out { format: global.output, color }, paths, config, registry, global })
    }

    pub fn network(&self) -> Result<NetworkProfile> {
        let id = self.global.network.clone().unwrap_or_else(|| self.config.default_network.clone());
        self.config.network(&id)
    }

    pub fn meta(&self) -> Result<WalletMeta> {
        self.registry.resolve(self.global.wallet.as_deref(), self.config.default_wallet.as_deref())
    }

    /// A session reading from the network's monitoring node when one is configured and checks
    /// out (broadcasts always go to the RPC endpoint).
    pub async fn session(&self) -> Result<Session> {
        let mut s = self.open_session()?;
        self.use_monitor(&mut s).await;
        Ok(s)
    }

    fn open_session(&self) -> Result<Session> {
        Session::open(self.registry.clone(), self.config.clone(), self.meta()?, self.network()?)
    }

    /// Point reads at the monitoring node, saying so when it is configured but cannot be used.
    async fn use_monitor(&self, s: &mut Session) {
        if let Some(why) = s.use_monitor().await
            && !self.out.json()
        {
            eprintln!("{}", self.out.dim(&format!("{why}; reading from the RPC endpoint")));
        }
    }

    pub async fn unlocked(&self) -> Result<Session> {
        Ok(self.unlocked_with_password().await?.0)
    }

    /// An unlocked session and the password that opened it, for the one command that needs it
    /// again (re-sealing the vault). It lives only as long as the command.
    pub async fn unlocked_with_password(&self) -> Result<(Session, zeroize::Zeroizing<String>)> {
        let mut s = self.open_session()?;
        if !s.meta.can_sign() {
            return Err(CoreError::Locked("this is a watch-only wallet; it cannot sign".into()));
        }
        let password = prompt::password(self.global.password_fd, &format!("Password for `{}`", s.meta.name))?;
        s.unlock(&password)?;
        // Every read goes to the monitoring node when it checks out; broadcasts stay on the RPC.
        self.config.require_execution_transport(&s.network)?;
        self.use_monitor(&mut s).await;
        Ok((s, password))
    }

    /// Show a review, ask for confirmation, then commit (or discard on rejection).
    pub async fn authorize(&self, session: &mut Session, mut review: Review) -> Result<Submitted> {
        let op = session.app.operation(&review.op_id)?.ok_or_else(|| CoreError::NotFound("review operation".into()))?;
        // Before anything is signed: is the node this review was read from keeping up?
        let lag = match session.lag_probe() {
            Some(probe) => probe.warning().await,
            None => None,
        };
        if let Some(warning) = &lag {
            review.warnings.insert(0, warning.clone());
        }
        let read_url = if session.monitoring() {
            session.network.monitor.as_ref().map(|m| m.rpc_url.as_str()).unwrap_or(&session.network.rpc_url)
        } else {
            &session.network.rpc_url
        };
        if self.out.json() {
            self.out.emit(
                "transaction review",
                &json!({
                    "review": review,
                    "wallet_id": session.meta.id,
                    "network_id": session.network.id,
                    "chain_id": session.network.chain_id,
                    "genesis": session.network.genesis,
                    "fee_base": op.fee,
                    "fee_store": op.store,
                    "sources": {
                        "read_origin": network::rpc_origin(read_url),
                        "broadcast_origin": network::rpc_origin(&session.network.rpc_url),
                        "read_transport": if read_url.starts_with("https:") { "tls" } else { "plaintext" },
                        "monitoring": session.monitoring(),
                        "monitor_lag_warning": lag.as_deref(),
                    },
                }),
            );
        } else {
            self.out.review(&review);
            eprintln!("  reads: {} · broadcast: {}", network::rpc_origin(read_url), network::rpc_origin(&session.network.rpc_url));
        }
        let validation = (|| -> Result<Option<String>> {
            self.config.require_execution_transport(&session.network)?;
            if let Some(path) = &self.global.authorization_policy {
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut std::io::Read::take(std::fs::File::open(path)?, 64 * 1024 + 1), &mut bytes)?;
                if bytes.len() > 64 * 1024 {
                    return Err(CoreError::Invalid("authorization policy exceeds 64 KiB".into()));
                }
                let policy: AuthorizationPolicy = serde_json::from_slice(&bytes)?;
                policy.check(&session.meta.id, &session.network, &op, &review, now())?;
            }
            match &review.confirm {
                // A risky review: the words replace the plain "yes".
                Some(phrase) => {
                    // The words given on the command line are the typed confirmation, checked by
                    // the commit like any other.
                    if let Some(words) = &self.global.confirm_words {
                        return Ok(Some(words.clone()));
                    }
                    if !self.global.yes {
                        eprintln!("  this review {}", review.risks.join("; "));
                        return Ok(Some(prompt::line(&format!("Type `{phrase}` to sign (anything else cancels)"))?));
                    }
                    // `--yes` alone never signs a risky review: the script must also say it accepts
                    // the risk, or hold an authorization policy naming this exact digest.
                    if self.global.accept_risk || self.global.authorization_policy.is_some() {
                        return Ok(Some(phrase.clone()));
                    }
                    Err(CoreError::Rejected(format!(
                        "this review needs its typed confirmation: add --confirm \"{phrase}\" (or --accept-risk) to --yes"
                    )))
                }
                None => prompt::confirm("Sign and broadcast?", "yes", self.global.yes).map(|()| None),
            }
        })();
        let typed = match validation {
            Ok(typed) => typed,
            Err(e) => {
                let _ = session.discard(&review.op_id);
                return Err(e);
            }
        };
        match session.commit_with(&review.op_id, typed.as_deref()).await {
            Err(e @ CoreError::Rejected(_)) if review.confirm.is_some() => {
                let _ = session.discard(&review.op_id);
                Err(e)
            }
            other => other,
        }
    }

    pub fn print_submitted(&self, command: &str, s: &Submitted) {
        if self.out.json() {
            self.out.emit(command, s);
            return;
        }
        let status = match s.status {
            OpStatus::Unknown => self.out.yellow("unknown"),
            _ => self.out.green(s.status.as_str()),
        };
        println!("{} {}", status, wallet_core::explorer::clean(&s.message, usize::MAX));
        println!("  operation {}", s.op_id);
        println!("  tx        {}", s.tx_hash);
        if let Some(url) = &s.explorer {
            println!("  explorer  {url}");
        }
    }
}

/// Optional automation capability. Exact signing digests bind calldata, token identities,
/// spender and nonce; the remaining scopes independently bound where and how much can execute.
/// Display symbols are deliberately absent from the authorization decision.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorizationPolicy {
    version: u32,
    wallet_id: String,
    network_id: String,
    chain_id: u64,
    genesis: String,
    accounts: Vec<String>,
    kinds: Vec<String>,
    destinations: Vec<String>,
    signing_digests: Vec<String>,
    fee_store: String,
    max_amount_base: String,
    max_fee_base: String,
    not_before: u64,
    expires_at: u64,
    max_review_age_secs: u64,
}

impl AuthorizationPolicy {
    fn check(&self, wallet: &str, network: &NetworkProfile, op: &wallet_core::appdb::Operation, review: &Review, time: u64) -> Result<()> {
        let has = |rows: &[String], value: &str| rows.iter().any(|v| v.eq_ignore_ascii_case(value));
        let digest = review.fields.iter().find(|f| f.label == "Signing digest").map(|f| f.value.as_str()).unwrap_or_default();
        let number = |raw: &str| {
            U256::from_str_radix(raw, 10).map_err(|_| CoreError::Invalid("authorization amounts must be decimal base-unit integers".into()))
        };
        if self.version != 1
            || self.wallet_id != wallet
            || self.network_id != network.id
            || self.chain_id != network.chain_id
            || !self.genesis.eq_ignore_ascii_case(&network.genesis)
            || self.network_id != op.network
            || !has(&self.accounts, &op.account)
            || !self.kinds.iter().any(|kind| kind == op.kind.as_str())
            || op.kind != review.kind
            || !has(&self.destinations, &review.to)
            || digest.is_empty()
            || !has(&self.signing_digests, digest)
            || self.fee_store != op.store
            || !matches!(op.store.as_str(), "quai" | "qi")
            || time < self.not_before
            || time >= self.expires_at
            || self.expires_at <= self.not_before
            || op.created > time
            || self.max_review_age_secs == 0
            || time - op.created > self.max_review_age_secs
            || review.fee_over_policy
            || op.status != OpStatus::Prepared
            || op.id != review.op_id
            || op.amount != review.amount_base
            || number(&op.amount)? > number(&self.max_amount_base)?
            || number(&op.fee)? > number(&self.max_fee_base)?
        {
            return Err(CoreError::Invalid("transaction is outside the supplied authorization policy".into()));
        }
        Ok(())
    }
}

fn q(v: U256) -> String {
    amount::group_thousands(&amount::format_amount_short(v, 18, 6))
}

fn qi(v: U256) -> String {
    amount::group_thousands(&amount::qi(v))
}

fn ts(secs: u64) -> String {
    let age = now().saturating_sub(secs);
    if age < 60 { "just now".into() } else { format!("{} ago", human_duration(age)) }
}

// ============================================================== wallet

pub async fn wallet(ctx: &mut Ctx, cmd: WalletCmd) -> Result<()> {
    match cmd {
        WalletCmd::Create { name, words, language, passphrase, skip_verify } => {
            let phrase = wallet_core::identity::generate_phrase(words, &language)?;
            let pass = if passphrase { prompt::new_secret("BIP39 passphrase", 1)? } else { zeroize::Zeroizing::new(String::new()) };
            let verified = if skip_verify { false } else { show_phrase_and_verify(ctx, &phrase)? };
            let password = new_password(ctx)?;
            let meta = ctx.registry.create_hd(&name, &phrase, &language, &pass, &password, verified)?;
            after_create(ctx, "wallet create", &meta)
        }
        WalletCmd::Import { name, from, file, language, passphrase, discover, allow_weak_kdf } => {
            let meta = match from {
                ImportKind::Mnemonic => {
                    let phrase = prompt::secret("Recovery phrase (words separated by spaces)")?;
                    wallet_core::identity::parse_mnemonic(&phrase, &language)?;
                    let pass = if passphrase { prompt::secret("BIP39 passphrase")? } else { zeroize::Zeroizing::new(String::new()) };
                    let password = new_password(ctx)?;
                    ctx.registry.create_hd(&name, &phrase, &language, &pass, &password, true)?
                }
                ImportKind::Key => {
                    let key = prompt::secret("Private key (hex)")?;
                    let password = new_password(ctx)?;
                    ctx.registry.create_from_key(&name, &key, &password)?
                }
                ImportKind::Keystore => {
                    let file = file.ok_or_else(|| CoreError::Invalid("--file is required for keystore import".into()))?;
                    let hex_key = decrypt_keystore(&file, allow_weak_kdf)?;
                    let password = new_password(ctx)?;
                    ctx.registry.create_from_key(&name, &hex_key, &password)?
                }
            };
            if discover && meta.kind == WalletKind::Hd {
                ctx.global.wallet = Some(meta.id.clone());
                let mut s = ctx.session().await?;
                let added = s.discover_quai_accounts(5).await?;
                if !ctx.out.json() {
                    println!("discovered {}", wallet_core::amount::count(added.len(), "additional account"));
                }
                s.scan_qi(None).await.ok();
            }
            after_create(ctx, "wallet import", &ctx.registry.resolve(Some(&meta.id), None)?)
        }
        WalletCmd::Watch { name, addresses } => {
            let pairs: Vec<(String, String)> = addresses.iter().enumerate().map(|(i, a)| (a.clone(), format!("Watch {}", i + 1))).collect();
            let meta = ctx.registry.create_watch(&name, &pairs)?;
            after_create(ctx, "wallet watch", &meta)
        }
        WalletCmd::List => {
            let wallets = ctx.registry.list()?;
            if ctx.out.json() {
                ctx.out.emit("wallet list", &wallets);
                return Ok(());
            }
            if wallets.is_empty() {
                println!("no wallets yet — run `quai-terminal wallet create --name NAME` or `wallet import`");
                return Ok(());
            }
            let default = ctx.config.default_wallet.clone();
            let rows = wallets
                .iter()
                .map(|w| {
                    vec![
                        if default.as_deref() == Some(w.name.as_str()) || default.as_deref() == Some(w.id.as_str()) {
                            "*".into()
                        } else {
                            " ".into()
                        },
                        w.name.clone(),
                        format!("{:?}", w.kind).to_lowercase(),
                        w.quai_accounts.iter().filter(|a| !a.archived).count().to_string(),
                        if w.kind == WalletKind::Hd { if w.backed_up { "yes".into() } else { ctx.out.yellow("no") } } else { "-".into() },
                        w.id.clone(),
                    ]
                })
                .collect::<Vec<_>>();
            ctx.out.table(&["", "name", "kind", "accounts", "backed up", "id"], &rows);
            Ok(())
        }
        WalletCmd::Show => {
            let meta = ctx.meta()?;
            if ctx.out.json() {
                ctx.out.emit("wallet show", &meta);
                return Ok(());
            }
            println!("{} ({:?})", ctx.out.bold(&meta.name), meta.kind);
            println!("  id              {}", meta.id);
            if let Some(words) = meta.word_count {
                println!("  recovery phrase {words} words{}", if meta.has_passphrase { " + passphrase" } else { "" });
                println!(
                    "  backed up       {}",
                    if meta.backed_up { "yes".into() } else { ctx.out.yellow("no — run `wallet verify-phrase`") }
                );
            }
            if let Some(code) = &meta.payment_code {
                println!("  payment code    {code}");
            }
            println!("  quai accounts   {}", meta.quai_accounts.len());
            println!("  imported qi     {}", meta.qi_imported.len());
            println!("  watch-only      {}", meta.watch.len());
            if ctx.registry.insecure_kdf() {
                println!("  {}", ctx.out.red("vault uses INSECURE development KDF parameters"));
            }
            Ok(())
        }
        WalletCmd::Rename { name } => {
            let mut meta = ctx.meta()?;
            let old = meta.name.clone();
            ctx.registry.rename(&mut meta, &name)?;
            if ctx.config.default_wallet.as_deref() == Some(old.as_str()) {
                ctx.config.default_wallet = Some(meta.name.clone());
                ctx.config.save(&ctx.paths)?;
            }
            done(ctx, "wallet rename", json!({"name": meta.name}), &format!("renamed to {}", meta.name))
        }
        WalletCmd::Delete { confirm } => {
            let meta = ctx.meta()?;
            let typed = match confirm {
                Some(c) => c,
                None => prompt::line(&format!(
                    "This permanently deletes `{}` from this computer. Funds are only recoverable from your recovery phrase or backup. Type the wallet name to delete",
                    meta.name
                ))?,
            };
            if typed != meta.name {
                return Err(CoreError::Rejected("name did not match; nothing deleted".into()));
            }
            ctx.registry.delete(&meta)?;
            if ctx.config.default_wallet.as_deref() == Some(meta.name.as_str()) {
                ctx.config.default_wallet = None;
                ctx.config.save(&ctx.paths)?;
            }
            done(ctx, "wallet delete", json!({"deleted": meta.id}), &format!("deleted {}", meta.name))
        }
        WalletCmd::ChangePassword => {
            let meta = ctx.meta()?;
            let old = prompt::password(ctx.global.password_fd, "Current password")?;
            ctx.registry.unlock(&meta, &old)?;
            let new = prompt::new_secret("New password", wallet_vault::MIN_PASSWORD_CHARS)?;
            ctx.registry.change_password(&meta, &old, &new)?;
            done(ctx, "wallet change-password", json!({"changed": true}), "password changed")
        }
        WalletCmd::ExportMnemonic => {
            let s = ctx.session().await?;
            if !ctx.global.yes {
                prompt::confirm("Your recovery phrase controls all funds. Make sure nobody can see your screen.", "reveal", false)?;
            }
            let password = prompt::password(ctx.global.password_fd, "Password")?;
            let (phrase, pass) = s.export_mnemonic(&password)?;
            if ctx.out.json() {
                ctx.out.emit("wallet export-mnemonic", &json!({"mnemonic": phrase.as_str(), "has_passphrase": !pass.is_empty()}));
            } else {
                println!();
                for (i, word) in phrase.split(' ').enumerate() {
                    print!("{:>2}. {:<12}", i + 1, word);
                    if (i + 1) % 4 == 0 {
                        println!();
                    }
                }
                println!();
                if !pass.is_empty() {
                    println!("{}", ctx.out.yellow("This wallet also uses a BIP39 passphrase (not shown)."));
                }
            }
            Ok(())
        }
        WalletCmd::ExportKey { address } => {
            let s = ctx.session().await?;
            if !ctx.global.yes {
                prompt::confirm("A private key controls that address's funds.", "reveal", false)?;
            }
            let password = prompt::password(ctx.global.password_fd, "Password")?;
            let key = s.export_private_key(&password, &address)?;
            if ctx.out.json() {
                ctx.out.emit("wallet export-key", &json!({"address": address, "private_key": key.as_str()}));
            } else {
                println!("{}", key.as_str());
            }
            Ok(())
        }
        WalletCmd::ExportKeystore { address, out } => {
            let s = ctx.session().await?;
            let password = prompt::password(ctx.global.password_fd, "Wallet password")?;
            let key_hex = s.export_private_key(&password, &address)?;
            let key = wallet_core::identity::parse_secret_hex(&key_hex)?;
            let ks_password = prompt::new_secret("Keystore password", 8)?;
            let encrypted = wallet_core::sdk::keystore::encrypt(&key, wallet_core::sdk::keystore::Password::Text(&ks_password))
                .map_err(|e| CoreError::Invalid(format!("keystore: {e}")))?;
            wallet_vault::write_private_atomic(&out, encrypted.as_json().as_bytes()).map_err(|e| CoreError::Storage(e.to_string()))?;
            done(ctx, "wallet export-keystore", json!({"file": out}), &format!("wrote {}", out.display()))
        }
        WalletCmd::ImportKey { label } => {
            let (mut s, password) = ctx.unlocked_with_password().await?;
            let key = prompt::secret("Private key (hex)")?;
            let address = s.import_key(&password, &key, &label)?;
            done(ctx, "wallet import-key", json!({"address": address}), &format!("imported {address}"))
        }
        WalletCmd::Backup { out } => {
            let meta = ctx.meta()?;
            let password = prompt::new_secret("Backup password", wallet_vault::MIN_PASSWORD_CHARS)?;
            let info = extras::create_backup(&ctx.registry, &ctx.config, &meta, &out, &password)?;
            done(
                ctx,
                "wallet backup",
                &info,
                &format!(
                    "encrypted backup written to {} ({})",
                    out.display(),
                    wallet_core::amount::count(info.networks.len(), "network state")
                ),
            )
        }
        WalletCmd::Restore { file } => {
            let password = prompt::secret("Backup password")?;
            let info = extras::restore_backup(&ctx.registry, &mut ctx.config, &ctx.paths, &file, &password)?;
            done(ctx, "wallet restore", &info, &format!("restored `{}` (your wallet password is unchanged)", info.wallet))
        }
        WalletCmd::VerifyBackup { file } => {
            let password = prompt::secret("Backup password")?;
            let info = extras::verify_backup(&ctx.registry, &file, &password)?;
            done(
                ctx,
                "wallet verify-backup",
                &info,
                &format!(
                    "backup OK: wallet `{}`, {}, networks: {}",
                    info.wallet,
                    wallet_core::amount::count(info.accounts, "account"),
                    info.networks.join(", ")
                ),
            )
        }
        WalletCmd::VerifyPhrase => {
            let mut meta = ctx.meta()?;
            let s = ctx.session().await?;
            let password = prompt::password(ctx.global.password_fd, "Password")?;
            let (phrase, _) = s.export_mnemonic(&password)?;
            quiz(ctx, &phrase)?;
            ctx.registry.update_meta(&mut meta, |current| {
                current.backed_up = true;
                Ok(())
            })?;
            done(ctx, "wallet verify-phrase", json!({"backed_up": true}), "recovery phrase verified")
        }
        WalletCmd::Use { name } => {
            let meta = ctx.registry.resolve(Some(&name), None)?;
            ctx.config.default_wallet = Some(meta.name.clone());
            ctx.config.save(&ctx.paths)?;
            done(ctx, "wallet use", json!({"default_wallet": meta.name}), &format!("default wallet is now {}", meta.name))
        }
    }
}

fn decrypt_keystore(file: &std::path::Path, allow_weak_kdf: bool) -> Result<zeroize::Zeroizing<String>> {
    use wallet_core::sdk::keystore::{KdfLimits, Keystore, KeystoreError, Password};
    // The default memory ceiling is exactly what geth's standard keystore (scrypt N 2^18, r 8)
    // needs, and the check refused it; an import the user started can afford twice that.
    let limits = KdfLimits::default().with_max_memory_bytes(512 * 1024 * 1024);
    let limits = if allow_weak_kdf { limits.without_strength_floors() } else { limits };
    let weak = |e: &KeystoreError| matches!(e, KeystoreError::WeakParameters);
    let refuse = || {
        CoreError::Rejected(
            "keystore: its password protection is weaker than this wallet accepts (scrypt N·r·p of at least 2^20, PBKDF2 of at \
             least 100,000 rounds, a salt of at least 16 bytes). If you trust the file, run the import again with \
             --allow-weak-kdf: the key is re-encrypted under this wallet's own password."
                .into(),
        )
    };
    let bytes = std::fs::read(file)?;
    let keystore =
        Keystore::from_json(&bytes, limits).map_err(|e| if weak(&e) { refuse() } else { CoreError::Invalid(format!("keystore: {e}")) })?;
    let pass = prompt::secret("Keystore password")?;
    let account = keystore
        .decrypt(Password::Text(&pass), limits)
        .map_err(|e| if weak(&e) { refuse() } else { CoreError::Locked(format!("keystore: {e}")) })?;
    Ok(zeroize::Zeroizing::new(hex_secret(account.secret_key()).to_string()))
}

fn hex_secret(key: &wallet_core::sdk::crypto::SecretKey) -> String {
    key.export_bytes().as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

fn new_password(ctx: &Ctx) -> Result<zeroize::Zeroizing<String>> {
    if let Some(fd) = ctx.global.password_fd {
        return prompt::password(Some(fd), "");
    }
    eprintln!("Choose a password to encrypt this wallet on this computer (min {} characters).", wallet_vault::MIN_PASSWORD_CHARS);
    prompt::new_secret("Wallet password", wallet_vault::MIN_PASSWORD_CHARS)
}

fn show_phrase_and_verify(ctx: &Ctx, phrase: &str) -> Result<bool> {
    if !prompt::interactive() {
        return Err(CoreError::Rejected(
            "wallet creation shows the recovery phrase and needs a terminal (or --skip-verify with --output json)".into(),
        ));
    }
    eprintln!();
    eprintln!("{}", ctx.out.bold("Write down your recovery phrase. Anyone with it controls your funds."));
    eprintln!();
    for (i, word) in phrase.split(' ').enumerate() {
        eprint!("{:>2}. {:<12}", i + 1, word);
        if (i + 1) % 4 == 0 {
            eprintln!();
        }
    }
    eprintln!();
    prompt::line("Press Enter when you have written it down")?;
    // Clear the phrase from the visible terminal before the quiz.
    eprint!("\x1b[2J\x1b[H");
    quiz(ctx, phrase)?;
    Ok(true)
}

fn quiz(ctx: &Ctx, phrase: &str) -> Result<()> {
    let words: Vec<&str> = phrase.split(' ').collect();
    let mut seed = [0u8; 8];
    wallet_core::sdk::crypto::fill_random(&mut seed).map_err(|_| CoreError::Storage("randomness".into()))?;
    let mut picks: Vec<usize> = Vec::new();
    let mut n = u64::from_le_bytes(seed);
    while picks.len() < 3 {
        let i = (n % words.len() as u64) as usize;
        n = n.rotate_left(17) ^ 0x9E37_79B9_7F4A_7C15;
        if !picks.contains(&i) {
            picks.push(i);
        }
    }
    picks.sort();
    for i in picks {
        let answer = prompt::line(&format!("Word #{}", i + 1))?;
        if answer.trim().to_lowercase() != words[i] {
            return Err(CoreError::Rejected(format!("word #{} did not match; verify later with `wallet verify-phrase`", i + 1)));
        }
    }
    eprintln!("{}", ctx.out.green("✓ recovery phrase verified"));
    Ok(())
}

fn after_create(ctx: &mut Ctx, command: &str, meta: &WalletMeta) -> Result<()> {
    if ctx.config.default_wallet.is_none() {
        ctx.config.default_wallet = Some(meta.name.clone());
        ctx.config.save(&ctx.paths)?;
    }
    if ctx.out.json() {
        ctx.out.emit(command, meta);
        return Ok(());
    }
    println!("{} wallet `{}` is ready", ctx.out.green("✓"), meta.name);
    if let Some(a) = meta.quai_accounts.first() {
        println!("  first Quai account  {}", a.address);
    }
    if let Some(code) = &meta.payment_code {
        println!("  Qi payment code     {code}");
    }
    if meta.kind == WalletKind::Hd && !meta.backed_up {
        println!("  {}", ctx.out.yellow("recovery phrase not verified yet — run `quai-terminal wallet verify-phrase`"));
    }
    Ok(())
}

fn done<T: serde::Serialize>(ctx: &Ctx, command: &str, value: T, message: &str) -> Result<()> {
    if ctx.out.json() {
        ctx.out.emit(command, &value);
    } else {
        println!("{} {message}", ctx.out.green("✓"));
    }
    Ok(())
}

// ============================================================== accounts & balances

pub async fn account(ctx: &Ctx, cmd: AccountCmd) -> Result<()> {
    match cmd {
        AccountCmd::List { all } => {
            let meta = ctx.meta()?;
            let active = meta.default_quai_account().ok().map(|a| a.address.clone());
            let is_active = |a: &wallet_core::registry::QuaiAccount| active.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(&a.address));
            let rows: Vec<_> = meta.quai_accounts.iter().filter(|a| all || !a.archived).collect();
            if ctx.out.json() {
                let rows: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|a| {
                        let mut v = serde_json::to_value(a).unwrap_or_default();
                        v["active"] = json!(is_active(a));
                        v
                    })
                    .collect();
                ctx.out.emit("account list", &rows);
                return Ok(());
            }
            let table = rows
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    vec![
                        if is_active(a) { format!("{}*", i + 1) } else { (i + 1).to_string() },
                        a.label.clone(),
                        a.address.clone(),
                        a.hd_index.map_or("imported".into(), |i| format!("m/44'/994'/0'/0/{i}")),
                        if a.archived { "archived".into() } else { String::new() },
                    ]
                })
                .collect::<Vec<_>>();
            ctx.out.table(&["#", "label", "address", "origin", ""], &table);
            Ok(())
        }
        AccountCmd::Add { label } => {
            let mut s = ctx.session().await?;
            let a = s.add_account(label.as_deref())?;
            done(ctx, "account add", &a, &format!("{} {}", a.label, a.address))
        }
        AccountCmd::Watch { address, label } => {
            let mut s = ctx.session().await?;
            let address = s.add_watch_address(&address, label.as_deref())?;
            done(ctx, "account watch", json!({"address": address}), &format!("watching {address}"))
        }
        AccountCmd::Use { account } => {
            let mut s = ctx.session().await?;
            let chosen = s.set_active_account(&account)?;
            done(
                ctx,
                "account use",
                json!({"active": chosen.address, "label": chosen.label}),
                &format!("{} acts from now on", chosen.label),
            )
        }
        AccountCmd::Rename { account, label } => {
            let mut s = ctx.session().await?;
            s.rename_account(&account, &label)?;
            done(ctx, "account rename", json!({"label": label}), "renamed")
        }
        AccountCmd::Archive { account } => {
            let mut s = ctx.session().await?;
            s.set_archived(&account, true)?;
            done(ctx, "account archive", json!({"archived": account}), "archived (custody records are kept)")
        }
        AccountCmd::Unarchive { account } => {
            let mut s = ctx.session().await?;
            s.set_archived(&account, false)?;
            done(ctx, "account unarchive", json!({"unarchived": account}), "restored")
        }
        AccountCmd::Discover { gap } => {
            let mut s = ctx.session().await?;
            let added = s.discover_quai_accounts(gap).await?;
            done(ctx, "account discover", &added, &format!("found {}", wallet_core::amount::count(added.len(), "new account")))
        }
    }
}

pub async fn balance(ctx: &Ctx, args: BalanceArgs) -> Result<()> {
    let mut s = ctx.session().await?;
    s.verify_node().await?;
    let balances = s.quai_balances().await?;
    if !args.no_refresh
        && (s.meta.qi_xpub.is_some() || !s.meta.qi_imported.is_empty())
        && let Err(e) = s.refresh_qi().await
    {
        eprintln!("{} Qi refresh failed: {e}", ctx.out.yellow("!"));
    }
    let qi_summary = s.qi_summary()?;
    let tokens = if args.tokens { Some(s.token_balances(None).await?) } else { None };
    // Prices describe mainnet QUAI only; test networks never show a fiat value.
    let price = if ctx.config.fetch_prices && s.network.chain_id == 9 { extras::cached_price(&s.app, true).await } else { None };
    if ctx.out.json() {
        ctx.out.emit(
            "balance",
            &json!({"network": s.network.id, "accounts": balances, "qi": qi_summary.balance, "qi_checkpoint": qi_summary.checkpoint_height, "tokens": tokens, "price": price}),
        );
        return Ok(());
    }
    let total = balances.iter().fold(U256::ZERO, |a, b| a.saturating_add(b.balance));
    println!("{} · {}", ctx.out.bold(&s.meta.name), s.network.name);
    println!();
    println!(
        "{}  {}{}",
        ctx.out.cyan("QUAI"),
        ctx.out.bold(&q(total)),
        price.as_ref().map_or(String::new(), |p| format!("  {}", ctx.out.dim(&format!("≈ {}", extras::usd_value(total, p)))))
    );
    let rows = balances
        .iter()
        .map(|b| {
            vec![
                b.label.clone(),
                short_address(&b.address),
                q(b.balance),
                if b.locked.is_zero() { String::new() } else { format!("{} locked", q(b.locked)) },
            ]
        })
        .collect::<Vec<_>>();
    ctx.out.table(&["  account", "address", "balance", ""], &rows);
    println!();
    let bal = qi_summary.balance;
    println!("{}    {}", ctx.out.magenta("Qi"), ctx.out.bold(&qi(bal.total)));
    println!("  spendable {}   locked {}   reserved {}", qi(bal.spendable), qi(bal.locked), qi(bal.reserved));
    match qi_summary.checkpoint_height {
        Some(h) => println!(
            "  {}",
            ctx.out.dim(&format!("snapshot at block {h} · {}", wallet_core::amount::count(qi_summary.coins.len(), "coin")))
        ),
        None if s.meta.qi_xpub.is_some() || !s.meta.qi_imported.is_empty() => {
            println!("  {}", ctx.out.dim("not scanned yet — run `quai-terminal qi scan`"))
        }
        None => {}
    }
    for (address, label, qits) in s.watch_qi_balances().await.unwrap_or_default() {
        println!("  {} {}  {} Qi  {}", ctx.out.dim("watch"), label, qi(qits), short_address(&address));
    }
    if let Some(tokens) = tokens {
        println!();
        let rows = tokens
            .iter()
            .map(|t| {
                vec![
                    t.token.symbol.clone(),
                    amount::group_thousands(&amount::format_amount_short(t.balance, t.token.decimals, 6)),
                    short_address(&t.token.address),
                ]
            })
            .collect::<Vec<_>>();
        ctx.out.table(&["token", "balance", "contract"], &rows);
    }
    Ok(())
}

pub async fn receive(ctx: &Ctx, args: ReceiveArgs) -> Result<()> {
    let mut s = ctx.session().await?;
    let (label, value, note) = match args.asset {
        Asset::Quai => {
            let a = s.account(args.account.as_deref())?;
            (format!("QUAI · {}", a.label), a.address.clone(), "Send only QUAI or Quai-network tokens to this address.".to_string())
        }
        Asset::Qi if args.address => {
            let address = s.new_qi_address(Some("receive"))?;
            (
                "Qi single-use address".into(),
                address.to_string(),
                "Each Qi output needs its own address; amounts needing several denominations should use your payment code.".into(),
            )
        }
        Asset::Qi => (
            "Qi payment code".into(),
            s.payment_code()?,
            "Share this reusable code; senders derive a fresh address for every payment.".into(),
        ),
    };
    if ctx.out.json() {
        ctx.out.emit("receive", &json!({"label": label, "value": value, "network": s.network.id}));
        return Ok(());
    }
    println!("{}", ctx.out.bold(&label));
    if args.qr {
        println!("{}", qr_text(&value));
    }
    println!("{value}");
    println!("{}", ctx.out.dim(&note));
    Ok(())
}

// ============================================================== sends & tokens

pub async fn send(ctx: &Ctx, cmd: SendCmd) -> Result<()> {
    let mut s = ctx.unlocked().await?;
    s.verify_node().await?;
    let (name, review) = match cmd {
        SendCmd::Quai { to, amount, from, fee } => {
            ("send quai", s.review_send_quai(from.as_deref(), &to, &amount, fee.max_fee.as_deref()).await?)
        }
        SendCmd::Token { token, to, amount, from, fee } => {
            ("send token", s.review_send_token(from.as_deref(), &token, &to, &amount, fee.max_fee.as_deref()).await?)
        }
        SendCmd::Batch { file, from, dry_run } => return send_batch(ctx, &mut s, &file, from.as_deref(), dry_run).await,
        SendCmd::Qi { to, amount, fee, notify } => {
            let review = s.review_send_qi(&to, &amount, fee.max_fee.as_deref()).await?;
            let needs_notify = review.warnings.iter().any(|w| w == wallet_core::ops::UNANNOUNCED);
            let submitted = ctx.authorize(&mut s, review).await?;
            ctx.print_submitted("send qi", &submitted);
            if notify && needs_notify {
                let r = s.review_notify(None, &to, None).await?;
                let n = ctx.authorize(&mut s, r).await?;
                ctx.print_submitted("payment notify", &n);
            }
            return Ok(());
        }
    };
    let submitted = ctx.authorize(&mut s, review).await?;
    ctx.print_submitted(name, &submitted);
    Ok(())
}

pub async fn token(ctx: &Ctx, cmd: TokenCmd) -> Result<()> {
    match cmd {
        TokenCmd::List => {
            let mut s = ctx.session().await?;
            s.ensure_default_tokens()?;
            let tokens = s.app.tokens(&s.network.id, false)?;
            if ctx.out.json() {
                ctx.out.emit("token list", &tokens);
                return Ok(());
            }
            let rows = tokens
                .iter()
                .map(|t| vec![t.symbol.clone(), t.name.clone(), t.decimals.to_string(), t.address.clone()])
                .collect::<Vec<_>>();
            ctx.out.table(&["symbol", "name", "decimals", "contract"], &rows);
            Ok(())
        }
        TokenCmd::Discover { .. } => unreachable!("handled in eco::token_discover"),
        TokenCmd::Import { address } => {
            let mut s = ctx.session().await?;
            let t = s.import_token(&address).await?;
            done(ctx, "token import", &t, &format!("imported {} ({}, {} decimals)", t.symbol, t.name, t.decimals))
        }
        TokenCmd::Remove { token } => {
            let s = ctx.session().await?;
            let t = s.app.token(&s.network.id, &token)?;
            s.app.remove_token(&s.network.id, &t.address)?;
            done(ctx, "token remove", json!({"removed": t.address}), &format!("removed {}", t.symbol))
        }
        TokenCmd::Balance { account } => {
            let mut s = ctx.session().await?;
            let balances = s.token_balances(account.as_deref()).await?;
            if ctx.out.json() {
                ctx.out.emit("token balance", &balances);
                return Ok(());
            }
            let rows = balances
                .iter()
                .map(|b| {
                    vec![
                        b.token.symbol.clone(),
                        amount::group_thousands(&amount::format_amount(b.balance, b.token.decimals)),
                        short_address(&b.owner),
                    ]
                })
                .collect::<Vec<_>>();
            ctx.out.table(&["token", "balance", "account"], &rows);
            Ok(())
        }
        TokenCmd::Allowance { token, spender, account } => {
            let mut s = ctx.session().await?;
            let (t, value) = s.token_allowance(account.as_deref(), &token, &spender).await?;
            let shown =
                if value == wallet_core::ops::UNLIMITED { "unlimited".to_string() } else { amount::format_amount(value, t.decimals) };
            done(
                ctx,
                "token allowance",
                json!({"token": t.address, "spender": spender, "allowance": value.to_string()}),
                &format!("{spender} may spend {shown} {}", t.symbol),
            )
        }
        TokenCmd::Approve { token, spender, amount, account, fee } => {
            let mut s = ctx.unlocked().await?;
            let review = s.review_approve(account.as_deref(), &token, &spender, amount.as_deref(), fee.max_fee.as_deref()).await?;
            let submitted = ctx.authorize(&mut s, review).await?;
            ctx.print_submitted("token approve", &submitted);
            Ok(())
        }
        TokenCmd::Revoke { token, spender, account, fee } => {
            let mut s = ctx.unlocked().await?;
            let review = s.review_approve(account.as_deref(), &token, &spender, Some("0"), fee.max_fee.as_deref()).await?;
            let submitted = ctx.authorize(&mut s, review).await?;
            ctx.print_submitted("token revoke", &submitted);
            Ok(())
        }
    }
}

// ============================================================== Qi, mining, payments

pub async fn qi_cmd(ctx: &Ctx, cmd: QiCmd) -> Result<()> {
    match cmd {
        QiCmd::Balance | QiCmd::Refresh => {
            let mut s = ctx.session().await?;
            let height = s.refresh_qi().await?;
            let summary = s.qi_summary()?;
            if ctx.out.json() {
                ctx.out.emit("qi balance", &json!({"height": height, "balance": summary.balance}));
                return Ok(());
            }
            let b = summary.balance;
            println!("Qi {} (block {height})", ctx.out.bold(&qi(b.total)));
            println!("  spendable {}  locked {}  reserved {}", qi(b.spendable), qi(b.locked), qi(b.reserved));
            Ok(())
        }
        QiCmd::Utxos => {
            let mut s = ctx.session().await?;
            let summary = s.qi_summary()?;
            if ctx.out.json() {
                ctx.out.emit("qi utxos", &summary);
                return Ok(());
            }
            let head = summary.checkpoint_height.unwrap_or(0);
            let rows = summary
                .coins
                .iter()
                .map(|c| {
                    let unlock = u64::try_from(c.unlock_height).unwrap_or(u64::MAX);
                    let state = if c.reserved {
                        "reserved".to_string()
                    } else if unlock > head {
                        format!("locked {}", human_duration((unlock - head) * wallet_core::track::BLOCK_SECS))
                    } else {
                        "spendable".into()
                    };
                    vec![
                        qi(U256::from(c.qits)),
                        state,
                        c.origin.clone(),
                        c.label.clone().unwrap_or_default(),
                        short_address(&c.address),
                        short_address(&c.outpoint),
                    ]
                })
                .collect::<Vec<_>>();
            ctx.out.table(&["qi", "state", "origin", "label", "address", "outpoint"], &rows);
            if summary.checkpoint_height.is_none() {
                println!("{}", ctx.out.dim("no snapshot yet — run `quai-terminal qi scan`"));
            }
            Ok(())
        }
        QiCmd::Scan { deep } => {
            let mut s = ctx.session().await?;
            let height = s.scan_qi(deep).await?;
            let summary = s.qi_summary()?;
            done(
                ctx,
                "qi scan",
                json!({"height": height, "balance": summary.balance, "coins": summary.coins.len()}),
                &format!(
                    "scanned at block {height}: {} Qi in {}{}",
                    qi(summary.balance.total),
                    wallet_core::amount::count(summary.coins.len(), "coin"),
                    if deep.is_some() { " (deep scan)" } else { "" }
                ),
            )
        }
        QiCmd::Sweep { destinations, quote, fee } => {
            if quote {
                let mut s = ctx.session().await?;
                let result = s.quote_sweep_qi(&destinations, fee.max_fee.as_deref()).await?;
                done(
                    ctx,
                    "qi sweep quote",
                    serde_json::to_value(&result)?,
                    &format!("{} qits received; {} qits fee", result.amount_qits, result.fee_qits),
                )
            } else {
                let mut s = ctx.unlocked().await?;
                let review = s.review_sweep_qi(&destinations, fee.max_fee.as_deref()).await?;
                let submitted = ctx.authorize(&mut s, review).await?;
                ctx.print_submitted("qi sweep", &submitted);
                Ok(())
            }
        }
        QiCmd::Consolidate { aggregate, fee } => {
            let mut s = ctx.unlocked().await?;
            let review = s.review_consolidate(aggregate, fee.max_fee.as_deref()).await?;
            let submitted = ctx.authorize(&mut s, review).await?;
            ctx.print_submitted("qi consolidate", &submitted);
            Ok(())
        }
        QiCmd::NewAddress { label } => {
            let mut s = ctx.session().await?;
            let a = s.new_qi_address(label.as_deref())?;
            done(ctx, "qi new-address", json!({"address": a.to_string()}), &a.to_string())
        }
        QiCmd::Addresses => {
            let s = ctx.session().await?;
            let list = s.qi_receive_addresses()?;
            if ctx.out.json() {
                ctx.out
                    .emit("qi addresses", &list.iter().map(|(i, a, l)| json!({"index": i, "address": a, "label": l})).collect::<Vec<_>>());
                return Ok(());
            }
            let rows = list.iter().map(|(i, a, l)| vec![i.to_string(), a.clone(), l.clone().unwrap_or_default()]).collect::<Vec<_>>();
            ctx.out.table(&["index", "address", "label"], &rows);
            Ok(())
        }
    }
}

pub async fn mining(ctx: &Ctx, cmd: MiningCmd) -> Result<()> {
    match cmd {
        MiningCmd::New { label } => {
            let mut s = ctx.session().await?;
            let a = s.new_qi_address(Some(&label))?;
            done(ctx, "mining new", json!({"address": a.to_string(), "label": label}), &format!("coinbase address {a} ({label})"))
        }
        MiningCmd::List => {
            let s = ctx.session().await?;
            let mut rows = Vec::new();
            for (i, a, l) in s.qi_receive_addresses()? {
                if l.as_deref().is_some_and(|l| l.to_lowercase().contains("min") || l.to_lowercase().contains("coinbase")) {
                    rows.push(vec![a, l.unwrap_or_default(), format!("receive #{i}")]);
                }
            }
            for imp in &s.meta.qi_imported {
                rows.push(vec![imp.address.clone(), imp.label.clone(), "imported key".into()]);
            }
            if ctx.out.json() {
                ctx.out.emit("mining list", &rows);
                return Ok(());
            }
            ctx.out.table(&["address", "label", "origin"], &rows);
            Ok(())
        }
        MiningCmd::Label { address, label } => {
            let s = ctx.session().await?;
            s.app.set_label(&address, &label)?;
            done(ctx, "mining label", json!({"address": address, "label": label}), "labeled")
        }
        MiningCmd::ImportKey { label } => {
            let (mut s, password) = ctx.unlocked_with_password().await?;
            let key = prompt::secret("Qi private key (hex)")?;
            let address = s.import_key(&password, &key, &label)?;
            done(ctx, "mining import-key", json!({"address": address}), &format!("imported {address}; run `qi refresh` to load its coins"))
        }
    }
}

pub async fn payment(ctx: &Ctx, cmd: PaymentCmd) -> Result<()> {
    match cmd {
        PaymentCmd::Code { qr } => {
            let s = ctx.session().await?;
            let code = s.payment_code()?;
            if ctx.out.json() {
                ctx.out.emit("payment code", &json!({"payment_code": code}));
            } else {
                if qr {
                    println!("{}", qr_text(&code));
                }
                println!("{code}");
            }
            Ok(())
        }
        PaymentCmd::Peers => {
            let s = ctx.unlocked().await?;
            let peers = s.peers()?;
            if ctx.out.json() {
                ctx.out.emit("payment peers", &peers);
                return Ok(());
            }
            let rows = peers
                .iter()
                .map(|p| {
                    vec![
                        p.contact.clone().unwrap_or_default(),
                        p.code.clone(),
                        p.receive_addresses.to_string(),
                        p.send_addresses.to_string(),
                    ]
                })
                .collect::<Vec<_>>();
            ctx.out.table(&["contact", "payment code", "receive addrs", "send addrs"], &rows);
            Ok(())
        }
        PaymentCmd::Add { code, name } => {
            let mut s = ctx.unlocked().await?;
            if let Some(name) = &name {
                s.app.add_contact(name, None, Some(code.trim()), "")?;
            }
            let (next, found) = s.scan_peer(&code, None).await?;
            done(
                ctx,
                "payment add",
                json!({"code": code, "scanned_through": next, "addresses": found}),
                &format!("peer added; scanned {found} address(es) through index {next}"),
            )
        }
        PaymentCmd::Scan { code, from } => {
            let mut s = ctx.unlocked().await?;
            let (next, found) = s.scan_peer(&code, from).await?;
            done(
                ctx,
                "payment scan",
                json!({"next_index": next, "addresses": found}),
                &format!("scanned {found} address(es); continue with --from {next}"),
            )
        }
        PaymentCmd::Discover => {
            let mut s = ctx.unlocked().await?;
            let summary = s.discover_mailbox().await?;
            let mut line = format!(
                "{} announced · {} rescanned",
                wallet_core::amount::count(summary.senders.len(), "sender"),
                wallet_core::amount::count(summary.registered.len(), "registered channel")
            );
            if summary.pending > 0 {
                line.push_str(&format!(" · {} waiting: payment offers", wallet_core::amount::count(summary.pending, "offer")));
            }
            if summary.refused > 0 {
                line.push_str(&format!(" · {} refused (no room for more channels)", summary.refused));
            }
            done(ctx, "payment discover", &summary, &line)
        }
        PaymentCmd::Offers => {
            let s = ctx.session().await?;
            let offers = s.channel_offers()?;
            if ctx.out.json() {
                ctx.out.emit("payment offers", &json!({"offers": offers}));
                return Ok(());
            }
            if offers.is_empty() {
                println!("no channel offers");
                return Ok(());
            }
            for o in &offers {
                println!("{}  {} Qi waiting (a lower bound)", o.code, wallet_core::amount::qi(o.found));
            }
            println!("accept with `payment accept CODE`, or `payment decline CODE`");
            Ok(())
        }
        PaymentCmd::Accept { code } => {
            let mut s = ctx.unlocked().await?;
            let (next, found) = s.accept_channel_offer(&code).await?;
            s.refresh_qi().await?;
            done(
                ctx,
                "payment accept",
                json!({"code": code, "next_index": next, "addresses": found}),
                &format!("channel registered; scanned {found} address(es) through index {next}"),
            )
        }
        PaymentCmd::Decline { code } => {
            let s = ctx.session().await?;
            s.decline_channel_offer(&code)?;
            done(ctx, "payment decline", json!({"code": code}), "declined; it will not be offered again")
        }
        PaymentCmd::Sync => {
            let mut s = ctx.unlocked().await?;
            let before = s.qi_summary()?.balance.total;
            let sync = s.sync_payment_channels().await?;
            let after = s.qi_summary()?.balance.total;
            let found = after.saturating_sub(before);
            done(
                ctx,
                "payment sync",
                json!({"channels_scanned": sync.scanned, "new_offers": sync.new_offers, "deferred": sync.deferred, "new_qits": found.to_string()}),
                &format!(
                    "scanned {}, {}, {} Qi newly found",
                    wallet_core::amount::count(sync.scanned, "channel"),
                    wallet_core::amount::count(sync.new_offers.len(), "new channel offer"),
                    wallet_core::amount::qi(found)
                ),
            )
        }
        PaymentCmd::Notify { peer, from, fee } => {
            let mut s = ctx.unlocked().await?;
            let review = s.review_notify(from.as_deref(), &peer, fee.max_fee.as_deref()).await?;
            let submitted = ctx.authorize(&mut s, review).await?;
            ctx.print_submitted("payment notify", &submitted);
            Ok(())
        }
    }
}

// ============================================================== conversions and wrapping

pub async fn convert(ctx: &Ctx, cmd: ConvertCmd) -> Result<()> {
    match cmd {
        ConvertCmd::Market { direction, amount, account, slippage, deadline, fee } => {
            let mut s = ctx.unlocked().await?;
            let wqi = s.network.wqi.clone().ok_or_else(|| CoreError::Invalid("WQI is not configured".into()))?;
            let direction = wallet_core::qi_market::Direction::parse(direction.key())?;
            crate::eco::run_action(
                ctx,
                &mut s,
                "market conversion",
                account.as_deref(),
                fee.max_fee.as_deref(),
                wallet_core::execution::TradingAction::MarketConversion {
                    direction,
                    amount,
                    stage: 0,
                    wqi,
                    slippage: slippage.unwrap_or(ctx.config.swap_slippage_bps),
                    deadline: deadline.unwrap_or(ctx.config.swap_deadline_minutes),
                    residual_atoms: "0".into(),
                },
            )
            .await?;
            Ok(())
        }
        ConvertCmd::MaxQuote { account, slippage, fee } => {
            let mut s = ctx.session().await?;
            let quote = s.quote_qi_special_max(false, account.as_deref(), slippage, fee.max_fee.as_deref()).await?;
            ctx.out.emit("convert max quote", &quote);
            Ok(())
        }
        ConvertCmd::Quote { direction, amount } => {
            let s = ctx.session().await?;
            let quote = s.conversion_quote(direction.key(), &amount).await?;
            if ctx.out.json() {
                ctx.out.emit("convert quote", &quote);
                return Ok(());
            }
            // The headline is the whole answer for most people: in, out, and what the discount
            // costs. Everything below it is for someone who wants to know why.
            println!("{}", ctx.out.bold(&quote.headline));
            // Shown only when the discount is visible; otherwise the rate can read below the
            // estimate, which looks like a gain and is only the two quotes' differing block basis.
            if let (Some(spot), Some(_)) = (&quote.quoted_display, quote.implied_slippage_bps.filter(|b| *b > 0)) {
                println!("  {}", ctx.out.dim(&format!("the rate alone is worth {spot}, before the controller's flow discount")));
            }
            if let Some(h) = &quote.hold {
                println!("  {}", ctx.out.red(&h.note));
            }
            if quote.discount_saturated {
                // Every scenario reads 90% here and the list teaches nothing.
                println!(
                    "  {}",
                    ctx.out.red(
                        "the discount is at its floor: this size pays one tenth of the rate whatever slippage you set. Convert a smaller amount, or take the market route below."
                    )
                );
            }
            if let Some(min) = &quote.minimum {
                println!("  minimum {min}");
            }
            if !quote.discount_saturated && !quote.scenarios.is_empty() {
                println!(
                    "  {}",
                    ctx.out.dim("if others convert in the same block (a discount above your slippage refunds the conversion):")
                );
                for sc in &quote.scenarios {
                    let over = sc.discount_bps > quote.suggested_slippage_bps;
                    let line =
                        format!("    {:>7}  {} (batch {} QUAI)", wallet_core::ops::percent(sc.discount_bps), sc.label, sc.batch_quai);
                    println!("{}", if over { ctx.out.red(&line) } else { line });
                }
            }
            if !quote.scenarios.is_empty() {
                let bps = quote.suggested_slippage_bps;
                let shown = format!("{} ({bps} bps)", wallet_core::ops::percent(bps));
                // At the floor the tolerance is not a lever on the loss; say what it still does, so
                // "set 90%" is not read as advice that 90% avoids anything.
                let tail = if quote.discount_saturated {
                    "  (the maximum: it avoids a refund on top of the discount, not the discount)"
                } else {
                    ""
                };
                println!("  slippage to send {}{}", ctx.out.bold(&shown), ctx.out.dim(tail));
            }
            if let Some(steps) = &quote.explorer_steps {
                println!("  {}", ctx.out.dim("explorer.qu.ai step preview (explanation only; the node estimate above is authoritative):"));
                for l in steps.lines() {
                    println!("    {l}");
                }
            }
            for n in &quote.notes {
                println!("  {}", ctx.out.dim(n));
            }
            // The other market: wrap, swap on Quainance, unwrap.
            let comparison = async {
                let data = s.data_ctx()?;
                let owner = s.meta.quai_accounts.first().map(|a| a.address.clone());
                let dir = wallet_core::qi_market::Direction::parse(direction.key())?;
                let base = wallet_core::amount::parse_amount(&amount, dir.pay_decimals())?;
                wallet_core::qi_market::compare(&data, dir, base, owner.as_deref(), s.config.swap_slippage_bps).await
            }
            .await;
            match comparison {
                Ok(c) => {
                    println!();
                    print_routes(ctx, &c);
                }
                Err(e) => println!("  {}", ctx.out.dim(&format!("market route unavailable: {e}"))),
            }
            Ok(())
        }
        ConvertCmd::QuaiToQi { amount, slippage, from, fee } => {
            let mut s = ctx.unlocked().await?;
            crate::eco::run_action(
                ctx,
                &mut s,
                "protocol QUAI to Qi",
                from.as_deref(),
                fee.max_fee.as_deref(),
                wallet_core::execution::TradingAction::ProtocolConversion {
                    direction: wallet_core::qi_market::Direction::QuaiToQi,
                    amount,
                    slippage,
                },
            )
            .await?;
            Ok(())
        }
        ConvertCmd::QiToQuai { amount, slippage, to, fee } => {
            let mut s = ctx.unlocked().await?;
            crate::eco::run_action(
                ctx,
                &mut s,
                "protocol Qi to QUAI",
                to.as_deref(),
                fee.max_fee.as_deref(),
                wallet_core::execution::TradingAction::ProtocolConversion {
                    direction: wallet_core::qi_market::Direction::QiToQuai,
                    amount,
                    slippage,
                },
            )
            .await?;
            Ok(())
        }
    }
}

pub async fn wrap(ctx: &Ctx, cmd: WrapCmd) -> Result<()> {
    if let WrapCmd::MaxQuote { account, fee } = &cmd {
        let mut s = ctx.session().await?;
        let quote = s.quote_qi_special_max(true, account.as_deref(), 0, fee.max_fee.as_deref()).await?;
        ctx.out.emit("wrap max quote", &quote);
        return Ok(());
    }
    if let WrapCmd::Status { account } = &cmd {
        let s = ctx.session().await?;
        let st = s.wrap_status(account.as_deref()).await?;
        if ctx.out.json() {
            ctx.out.emit("wrap status", &st);
            return Ok(());
        }
        println!("{}", ctx.out.bold(&st.account));
        if let Some(v) = &st.wqi_qi {
            println!("  WQI        {v} Qi");
        }
        if let Some(v) = &st.unclaimed_qits {
            let u: U256 = v.parse().unwrap_or_default();
            println!(
                "  unclaimed  {} Qi{}",
                qi(u),
                if u.is_zero() { String::new() } else { format!("  {}", ctx.out.yellow("→ `wrap claim`")) }
            );
        }
        if let Some(v) = &st.wquai_atoms {
            println!("  WQUAI      {}", q(v.parse().unwrap_or_default()));
        }
        return Ok(());
    }
    let mut s = ctx.unlocked().await?;
    let (name, review) = match cmd {
        WrapCmd::Qi { amount, account, fee } => ("wrap qi", s.review_wrap_qi(account.as_deref(), &amount, fee.max_fee.as_deref()).await?),
        WrapCmd::Claim { account, fee } => ("wrap claim", s.review_claim_wqi(account.as_deref(), fee.max_fee.as_deref()).await?),
        WrapCmd::UnwrapQi { amount, account, fee } => {
            ("wrap unwrap-qi", s.review_unwrap_wqi(account.as_deref(), &amount, fee.max_fee.as_deref()).await?)
        }
        WrapCmd::Quai { amount, account, fee } => {
            ("wrap quai", s.review_wrap_quai(account.as_deref(), &amount, fee.max_fee.as_deref()).await?)
        }
        WrapCmd::UnwrapQuai { amount, account, fee } => {
            ("wrap unwrap-quai", s.review_unwrap_quai(account.as_deref(), &amount, fee.max_fee.as_deref()).await?)
        }
        WrapCmd::Status { .. } | WrapCmd::MaxQuote { .. } => unreachable!(),
    };
    let submitted = ctx.authorize(&mut s, review).await?;
    ctx.print_submitted(name, &submitted);
    Ok(())
}

// ============================================================== tx, history, locks

pub async fn tx(ctx: &Ctx, cmd: TxCmd) -> Result<()> {
    match cmd {
        TxCmd::List { limit } => {
            let s = ctx.session().await?;
            let ops = s.app.operations(&s.network.id, limit)?;
            if ctx.out.json() {
                ctx.out.emit("tx list", &ops);
                return Ok(());
            }
            let rows = ops
                .iter()
                .map(|o| {
                    vec![
                        o.id[..8].to_string(),
                        ts(o.created),
                        status_text(ctx, o.status),
                        describe(o),
                        short_address(&o.counterparty),
                        o.tx_hash.as_deref().map(short_address).unwrap_or_default(),
                    ]
                })
                .collect::<Vec<_>>();
            ctx.out.table(&["id", "when", "status", "operation", "to", "tx"], &rows);
            Ok(())
        }
        TxCmd::Cancel { id } => {
            let mut s = ctx.session().await?;
            let op = s.abandon(&id)?;
            done(
                ctx,
                "tx cancel",
                &op,
                &format!("released the nonce {} reserved; queued transactions can be mined", &op.id[..8.min(op.id.len())]),
            )
        }
        TxCmd::Show { id } => {
            let s = ctx.session().await?;
            let op = s.app.find_operation(&s.network.id, &id)?;
            if ctx.out.json() {
                ctx.out.emit("tx show", &op);
                return Ok(());
            }
            println!("{} {}", ctx.out.bold(&describe(&op)), status_text(ctx, op.status));
            println!("  operation  {}", op.id);
            println!("  kind       {}", op.kind);
            println!("  from       {}", op.account);
            println!("  to         {}", op.counterparty);
            if let Some(h) = &op.tx_hash {
                println!("  tx         {h}");
                if let Some(url) = s.network.tx_url(h) {
                    println!("  explorer   {url}");
                }
            }
            if !op.fee.is_empty() {
                let fee: U256 = op.fee.parse().unwrap_or_default();
                println!(
                    "  fee        {}",
                    if op.store == "qi" { format!("{} Qi", amount::qi(fee)) } else { format!("{} QUAI", amount::quai(fee)) }
                );
            }
            println!("  created    {}", ts(op.created));
            for (k, v) in op.detail.entries() {
                println!("  {:<10} {}", k, v);
            }
            Ok(())
        }
        TxCmd::Track => {
            let mut s = ctx.session().await?;
            let report = s.track().await?;
            if ctx.out.json() {
                ctx.out.emit("tx track", &report);
                return Ok(());
            }
            for c in &report.changes {
                println!("{} {} → {}  {}", &c.op_id[..8], c.from.as_str(), status_text(ctx, c.to), c.message);
            }
            for a in &report.incoming {
                let amt: U256 = a.amount.parse().unwrap_or_default();
                let shown = if a.asset == "QI" { format!("{} Qi", qi(amt)) } else { format!("{} QUAI", q(amt)) };
                println!("{} {} {shown} at {}", ctx.out.green("↘"), wallet_core::track::incoming_verb(a), short_address(&a.address));
            }
            for e in &report.errors {
                eprintln!("{} {e}", ctx.out.yellow("!"));
            }
            if report.changes.is_empty() && report.incoming.is_empty() {
                println!("{}", ctx.out.dim("no changes"));
            }
            Ok(())
        }
        TxCmd::Wait { id, timeout } => {
            let mut s = ctx.session().await?;
            let started = std::time::Instant::now();
            loop {
                let op = s.app.find_operation(&s.network.id, &id)?;
                if !matches!(op.status, OpStatus::Signed | OpStatus::Submitted | OpStatus::Unknown | OpStatus::Prepared) {
                    return done(ctx, "tx wait", &op, &format!("{} {}", describe(&op), op.status.as_str()));
                }
                if started.elapsed().as_secs() >= timeout {
                    return Err(CoreError::Timeout(format!("operation still {} after {timeout}s", op.status.as_str())));
                }
                let _ = s.track().await;
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        }
        TxCmd::Rebroadcast { id } => {
            let mut s = if ctx.session().await?.app.find_operation(&ctx.network()?.id, &id)?.store == "qi" {
                ctx.unlocked().await?
            } else {
                ctx.session().await?
            };
            let submitted = s.rebroadcast(&id).await?;
            ctx.print_submitted("tx rebroadcast", &submitted);
            Ok(())
        }
        TxCmd::Speedup { id, bump } => {
            let mut s = ctx.unlocked().await?;
            let review = s.prepare_speed_up(&id, bump).await?;
            let submitted = ctx.authorize(&mut s, review).await?;
            ctx.print_submitted("tx speedup", &submitted);
            Ok(())
        }
        TxCmd::FillGap { from } => {
            let mut s = ctx.unlocked().await?;
            let review = s.review_fill_gap(from.as_deref()).await?;
            let submitted = ctx.authorize(&mut s, review).await?;
            ctx.print_submitted("tx fill-gap", &submitted);
            Ok(())
        }
    }
}

fn status_text(ctx: &Ctx, status: OpStatus) -> String {
    let t = status.as_str();
    match status {
        OpStatus::Confirmed | OpStatus::Settled => ctx.out.green(t),
        OpStatus::Failed => ctx.out.red(t),
        OpStatus::Refunded | OpStatus::Unknown => ctx.out.yellow(t),
        OpStatus::Locked | OpStatus::Settling | OpStatus::Submitted | OpStatus::Signed => ctx.out.cyan(t),
        _ => ctx.out.dim(t),
    }
}

pub async fn history(ctx: &Ctx, args: HistoryArgs) -> Result<()> {
    let s = ctx.session().await?;
    if args.clear {
        prompt::confirm(
            "Clear displayed history for this network? Pending operations and custody records are kept.",
            "clear",
            ctx.global.yes,
        )?;
        let n = s.app.clear_history(&s.network.id)?;
        return done(ctx, "history", json!({"cleared": n}), &format!("cleared {n} entries"));
    }
    let ops = s.app.operations(&s.network.id, args.limit)?;
    let activity = s.app.activity(&s.network.id, args.limit)?;
    if ctx.out.json() {
        ctx.out.emit("history", &json!({"operations": ops, "received": activity}));
        return Ok(());
    }
    let mut rows: Vec<(u64, Vec<String>)> = ops
        .iter()
        .filter(|o| o.status != OpStatus::Cancelled)
        .map(|o| (o.created, vec![ts(o.created), "↗".into(), describe(o), status_text(ctx, o.status), short_address(&o.counterparty)]))
        .collect();
    for a in &activity {
        let amt: U256 = a.amount.parse().unwrap_or_default();
        let shown = if a.asset == "QI" { format!("{} Qi", qi(amt)) } else { format!("{} QUAI", q(amt)) };
        rows.push((
            a.observed,
            vec![
                ts(a.observed),
                ctx.out.green("↘"),
                format!("{} {shown}", wallet_core::track::incoming_verb(a)),
                "observed".into(),
                short_address(&a.address),
            ],
        ));
    }
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    let rows: Vec<Vec<String>> = rows.into_iter().take(args.limit as usize).map(|r| r.1).collect();
    ctx.out.table(&["when", "", "activity", "status", "address"], &rows);
    Ok(())
}

pub async fn locks(ctx: &Ctx) -> Result<()> {
    let mut s = ctx.session().await?;
    let _ = s.refresh_qi().await;
    let items = s.locks().await?;
    if ctx.out.json() {
        ctx.out.emit("locks", &items);
        return Ok(());
    }
    if items.is_empty() {
        println!("{}", ctx.out.dim("nothing is locked"));
        return Ok(());
    }
    let rows = items
        .iter()
        .map(|i| {
            vec![
                i.source.clone(),
                format!("{} {}", i.amount, i.asset),
                i.unlock_height.map(|h| h.to_string()).unwrap_or_else(|| "—".into()),
                i.blocks_remaining.map(|b| wallet_core::amount::count(b, "block")).unwrap_or_default(),
                i.eta_secs.map(|e| format!("~{}", human_duration(e))).unwrap_or_default(),
            ]
        })
        .collect::<Vec<_>>();
    ctx.out.table(&["source", "amount", "unlock block", "remaining", "eta"], &rows);
    Ok(())
}

// ============================================================== contacts, networks, config

pub async fn contact(ctx: &Ctx, cmd: ContactCmd) -> Result<()> {
    let mut s = ctx.session().await?;
    match cmd {
        ContactCmd::Add { name, address, payment_code, note } => {
            let c = s.save_contact(None, &name, address.as_deref(), payment_code.as_deref(), &note)?;
            done(ctx, "contact add", &c, &format!("added {}", c.name))
        }
        ContactCmd::Edit { name, rename, address, payment_code, note, clear_address, clear_payment_code } => {
            let current = s.app.contact(&name)?.ok_or_else(|| CoreError::NotFound(format!("no contact `{name}`")))?;
            let address = if clear_address { None } else { address.or(current.address) };
            let code = if clear_payment_code { None } else { payment_code.or(current.payment_code) };
            let c = s.save_contact(
                Some(&name),
                rename.as_deref().unwrap_or(&name),
                address.as_deref(),
                code.as_deref(),
                note.as_deref().unwrap_or(&current.note),
            )?;
            done(ctx, "contact edit", &c, &format!("updated {}", c.name))
        }
        ContactCmd::Save { name, value } => {
            let plan = s.plan_contact_save(&name, &value)?;
            if plan.unchanged {
                return done(ctx, "contact save", &plan, &format!("{} already has that {}", plan.contact, plan.value.describe()));
            }
            if !plan.warnings.is_empty() {
                if !ctx.out.json() {
                    for w in &plan.warnings {
                        println!("{} {w}", ctx.out.yellow("!"));
                    }
                }
                prompt::confirm(&format!("Save it to {}?", plan.contact), "yes", ctx.global.yes)?;
            }
            let (c, plan) = s.save_to_contact(&name, &value)?;
            done(ctx, "contact save", &plan, &format!("saved {} to {}", plan.value.describe(), c.name))
        }
        ContactCmd::Forget { name, account } => {
            let c = s.forget_contact_account(&name, &account)?;
            done(ctx, "contact forget", &c, &format!("{} no longer has {}", c.name, wallet_core::session::short_address(&account)))
        }
        ContactCmd::List => {
            let contacts = s.app.contacts()?;
            let accounts = |c: &wallet_core::appdb::Contact| -> Vec<String> {
                let mut all: Vec<String> = c.address.iter().cloned().collect();
                for a in s.app.contact_addresses(c.id).unwrap_or_default() {
                    if !all.iter().any(|x| x.eq_ignore_ascii_case(&a)) {
                        all.push(a);
                    }
                }
                all
            };
            if ctx.out.json() {
                let rows: Vec<serde_json::Value> = contacts
                    .iter()
                    .map(|c| {
                        let mut v = serde_json::to_value(c).unwrap_or_default();
                        v["accounts"] = json!(accounts(c));
                        v
                    })
                    .collect();
                ctx.out.emit("contact list", &rows);
                return Ok(());
            }
            let rows = contacts
                .iter()
                .map(|c| {
                    let all = accounts(c);
                    let shown = match all.split_first() {
                        Some((first, rest)) if !rest.is_empty() => format!("{first} (+{} more)", rest.len()),
                        Some((first, _)) => first.clone(),
                        None => String::new(),
                    };
                    vec![
                        c.name.clone(),
                        shown,
                        c.payment_code.as_deref().map(wallet_core::session::short_code).unwrap_or_default(),
                        c.note.clone(),
                    ]
                })
                .collect::<Vec<_>>();
            ctx.out.table(&["name", "address", "payment code", "note"], &rows);
            Ok(())
        }
        ContactCmd::Remove { name } => {
            if !s.app.remove_contact(&name)? {
                return Err(CoreError::NotFound(format!("no contact `{name}`")));
            }
            done(ctx, "contact remove", json!({"removed": name}), "removed")
        }
    }
}

pub async fn network_cmd(ctx: &mut Ctx, cmd: NetworkCmd) -> Result<()> {
    match cmd {
        NetworkCmd::List => {
            let nets = ctx.config.networks();
            if ctx.out.json() {
                ctx.out.emit("network list", &nets);
                return Ok(());
            }
            let rows = nets
                .iter()
                .map(|n| {
                    vec![
                        if n.id == ctx.config.default_network { "*".into() } else { " ".into() },
                        n.id.clone(),
                        n.name.clone(),
                        n.chain_id.to_string(),
                        n.rpc_url.clone(),
                        if n.builtin { "built-in".into() } else { "custom".into() },
                    ]
                })
                .collect::<Vec<_>>();
            ctx.out.table(&["", "id", "name", "chain", "rpc", ""], &rows);
            Ok(())
        }
        NetworkCmd::Health => {
            let profile = ctx.network()?;
            let node = profile.node()?;
            let health = network::check_node(&profile, &node).await?;
            if ctx.out.json() {
                ctx.out.emit("network health", &health);
                return Ok(());
            }
            println!(
                "{} {}",
                ctx.out.bold(&profile.name),
                if health.identity_ok { ctx.out.green("● identity ok") } else { ctx.out.red("✕ identity mismatch") }
            );
            println!(
                "  block       {}{}",
                health.height,
                health.head_age_secs.map_or(String::new(), |a| format!("  ({} ago)", human_duration(a)))
            );
            println!("  latency     {} ms", health.latency_ms);
            println!("  gas price   {} wei", health.gas_price);
            if let Some(v) = &health.client_version {
                println!("  client      {v}");
            }
            println!(
                "  fees        {}",
                if profile.specialized_fee_estimation {
                    "specialized Qi fee estimation (v0.56 profile)"
                } else {
                    "explicit specialized Qi fees"
                }
            );
            if !health.identity_ok {
                return Err(CoreError::Network("node identity does not match the trusted profile".into()));
            }
            Ok(())
        }
        NetworkCmd::Add {
            id,
            rpc,
            chain_id,
            genesis,
            trust_node_genesis,
            pathing,
            name,
            specialized_fees,
            wqi,
            wquai,
            mailbox,
            messages,
            explorer,
            explorer_api,
            explorer_kind,
            router,
            factory,
            usdt,
            zora_asks,
            zora_manager,
            zora_erc721_helper,
            zora_erc20_helper,
            listings_indexer,
            max_gas_price,
            max_total_fee,
        } => {
            let pin = |a: Option<String>| a.map(|address| wallet_core::network::PinnedContract { address, code_hash: None });
            let ecosystem = wallet_core::network::Ecosystem {
                quainance_router: pin(router),
                quainance_factory: pin(factory),
                usdt: pin(usdt),
                zora_asks: pin(zora_asks),
                zora_module_manager: pin(zora_manager),
                zora_erc721_helper: pin(zora_erc721_helper),
                zora_erc20_helper: pin(zora_erc20_helper),
                messages: pin(messages),
                bazarr_indexer: listings_indexer,
                ..Default::default()
            };
            let explorer_api = explorer_api.map(|base_url| wallet_core::network::ExplorerApiConfig {
                kind: match explorer_kind {
                    ExplorerFlavor::Quai => wallet_core::network::ExplorerKind::QuaiExplorer,
                    ExplorerFlavor::Blockscout => wallet_core::network::ExplorerKind::Blockscout,
                },
                base_url,
            });
            let max_total_fee = amount::parse_quai(&max_total_fee)?.to_string();
            let genesis = match (genesis, trust_node_genesis) {
                (Some(g), _) => g,
                (None, true) => {
                    let mut probe = NetworkProfile::builtins()[0].clone();
                    probe.chain_id = chain_id;
                    probe.rpc_url = rpc.clone();
                    probe.use_pathing = pathing;
                    let provider = probe.provider()?;
                    provider.genesis_hash(network::ZONE).await?.to_string()
                }
                (None, false) => return Err(CoreError::Invalid("provide --genesis HASH or --trust-node-genesis".into())),
            };
            let profile = NetworkProfile {
                id: id.clone(),
                name: name.unwrap_or_else(|| id.clone()),
                chain_id,
                genesis,
                rpc_url: rpc,
                use_pathing: pathing,
                ws_url: None,
                pinned_latest: true,
                specialized_fee_estimation: specialized_fees,
                wqi,
                wquai,
                monitor: None,
                mailbox,
                explorer,
                explorer_api,
                ecosystem,
                max_gas_price,
                max_total_fee,
                builtin: false,
            };
            profile.validate()?;
            if ctx.config.networks().iter().any(|n| n.id == id) {
                return Err(CoreError::Invalid(format!("network `{id}` already exists")));
            }
            ctx.config.networks.push(profile.clone());
            ctx.config.save(&ctx.paths)?;
            done(ctx, "network add", &profile, &format!("added network {id} (genesis {})", profile.genesis))
        }
        NetworkCmd::Remove { id } => {
            let before = ctx.config.networks.len();
            ctx.config.networks.retain(|n| n.id != id);
            if before == ctx.config.networks.len() {
                return Err(CoreError::NotFound(format!("no custom network `{id}`")));
            }
            ctx.config.save(&ctx.paths)?;
            done(ctx, "network remove", json!({"removed": id}), "removed (wallet state for it is kept on disk)")
        }
        NetworkCmd::Transport { id, allow_insecure } => {
            ctx.config.network(&id)?;
            if allow_insecure {
                ctx.config.allow_insecure_rpc.insert(id.clone());
            } else {
                ctx.config.allow_insecure_rpc.remove(&id);
            }
            ctx.config.save(&ctx.paths)?;
            done(
                ctx,
                "network transport",
                json!({"network": id, "allow_insecure_rpc": allow_insecure}),
                if allow_insecure {
                    "remote plaintext RPC explicitly allowed for this network"
                } else {
                    "remote execution RPC requires HTTPS"
                },
            )
        }
        NetworkCmd::Monitor { id, url, pathing, clear, trust_execution, allow_insecure_rpc } => {
            let mut profile = ctx.config.network(&id)?;
            if clear {
                ctx.config.monitor_endpoints.remove(&id);
                ctx.config.execution_monitor_trust.remove(&id);
                ctx.config.save(&ctx.paths)?;
                return done(ctx, "network monitor", json!({"network": id, "monitor": null}), "monitoring uses the main RPC again");
            }
            let Some(url) = url else {
                let current = profile.monitor.as_ref().map(|m| m.rpc_url.clone());
                return done(
                    ctx,
                    "network monitor",
                    json!({"network": id, "monitor": current, "serves_reviews": ctx.config.monitor_serves_execution(&profile), "lag_warning_blocks": network::MONITOR_LAG_WARN, "allow_insecure_rpc": ctx.config.allow_insecure_rpc.contains(&id)}),
                    &match &current {
                        Some(u) => format!(
                            "monitoring endpoint: {u}, for every read (broadcasts use {}; a review warns when it is {} or more blocks behind)",
                            profile.rpc_url,
                            network::MONITOR_LAG_WARN
                        ),
                        None => format!("no monitoring endpoint; everything uses {}", profile.rpc_url),
                    },
                );
            };
            let endpoint = network::MonitorEndpoint { rpc_url: url.clone(), use_pathing: pathing };
            profile.monitor = Some(endpoint.clone());
            // Fail closed: the endpoint must be the same chain before anything reads from it.
            let node = profile.monitor_node()?;
            network::require_identity(&profile, &node.provider).await?;
            let _ = trust_execution;
            if allow_insecure_rpc {
                ctx.config.allow_insecure_rpc.insert(id.clone());
            }
            // Reviews read from it, so it needs the transport a review's reads need: TLS, or this
            // machine or the local network, unless remote plaintext was allowed.
            network::require_secure_rpc(&url, ctx.config.allow_insecure_rpc.contains(&id))?;
            ctx.config.monitor_endpoints.insert(id.clone(), endpoint);
            ctx.config.execution_monitor_trust.remove(&id);
            ctx.config.save(&ctx.paths)?;
            done(
                ctx,
                "network monitor",
                json!({"network": id, "monitor": url, "serves_reviews": true, "lag_warning_blocks": network::MONITOR_LAG_WARN}),
                &format!(
                    "monitoring endpoint set for {id} (chain id and genesis verified): every read uses it, transactions are broadcast through {}, and a review warns when it is {} or more blocks behind",
                    profile.rpc_url,
                    network::MONITOR_LAG_WARN
                ),
            )
        }
        NetworkCmd::Use { id } => {
            ctx.config.network(&id)?;
            ctx.config.default_network = id.clone();
            ctx.config.save(&ctx.paths)?;
            done(ctx, "network use", json!({"default_network": id}), &format!("default network is now {id}"))
        }
    }
}

pub async fn config_cmd(ctx: &mut Ctx, cmd: ConfigCmd) -> Result<()> {
    match cmd {
        ConfigCmd::Show => {
            if ctx.out.json() {
                ctx.out.emit("config show", &ctx.config);
            } else {
                print!("{}", toml::to_string_pretty(&ctx.config).unwrap_or_default());
                println!("# file: {}", ctx.paths.config_file().display());
            }
            Ok(())
        }
        ConfigCmd::Set { key, value } if key == "ipfs_gateway" => {
            // Tested before it is saved, the same as in Settings: refused when unreachable or when
            // it returns content that does not match the CID asked for.
            ctx.config.set(&key, &value)?;
            let gateway = wallet_core::ipfs::Gateway::parse(ctx.config.ipfs_gateway.as_deref().unwrap_or_default())?;
            let outcome = wallet_core::ipfs::test(&gateway).await?;
            ctx.config.save(&ctx.paths)?;
            let stored = gateway.display();
            let note = match &outcome {
                wallet_core::ipfs::TestOutcome::Verified(ms) => format!("test file verified against its CID in {ms} ms"),
                wallet_core::ipfs::TestOutcome::Answered(why) => format!("reachable, but the test file did not arrive ({why})"),
            };
            done(ctx, "config set", json!({"ipfs_gateway": stored, "test": note}), &format!("{key} = {stored} · {note}"))
        }
        ConfigCmd::Set { key, value } => {
            ctx.config.set(&key, &value)?;
            ctx.config.save(&ctx.paths)?;
            done(ctx, "config set", json!({key.clone(): value}), &format!("{key} = {value}"))
        }
    }
}

pub async fn notifications(ctx: &Ctx, args: NotificationsArgs) -> Result<()> {
    let s = ctx.session().await?;
    let list = s.app.notifications(args.limit)?;
    if args.read {
        s.app.mark_notifications_read()?;
    }
    if ctx.out.json() {
        ctx.out.emit("notifications", &list);
        return Ok(());
    }
    for n in &list {
        let mark = if n.read { " " } else { "•" };
        println!("{mark} {:<10} {:<28} {}", ts(n.at), n.title, n.body);
    }
    if list.is_empty() {
        println!("{}", ctx.out.dim("no notifications"));
    }
    Ok(())
}

pub async fn status(ctx: &Ctx, args: StatusArgs) -> Result<()> {
    let status = if args.live {
        let mut s = ctx.session().await?;
        let st = s.public_status(args.show_balances).await;
        Some(st)
    } else {
        extras::read_status(&ctx.paths).map(|mut st| {
            if !args.show_balances {
                st.balances = None;
            }
            // A status file left by a daemon that is no longer running must not claim "unlocked".
            if !crate::daemon::daemon_running(&ctx.paths) {
                st.unlocked = false;
            }
            st
        })
    };
    let status = match status {
        Some(s) => s,
        None => {
            let mut s = ctx.session().await?;
            s.public_status(args.show_balances).await
        }
    };
    match args.format {
        StatusFormat::Waybar => println!("{}", status.waybar()),
        StatusFormat::Json => println!("{}", serde_json::to_string(&status).unwrap_or_default()),
        StatusFormat::Human => {
            println!(
                "{} on {} · {} · block {} · {} · {} attention",
                status.wallet,
                status.network,
                if status.node_ok { "node ok" } else { "node unreachable" },
                status.height.map_or("?".into(), |h| h.to_string()),
                if status.unlocked { "unlocked" } else { "locked" },
                status.attention
            );
            if let Some(b) = status.balances {
                println!("{b}");
            }
        }
    }
    Ok(())
}

/// Both markets for a QUAI ⇄ Qi trade, the better-paying one first.
pub fn print_routes(ctx: &Ctx, c: &wallet_core::qi_market::Comparison) {
    let best = c.better().map(|r| r.name.clone());
    println!("{} of {}:", ctx.out.bold("routes"), c.amount_display);
    for route in [&c.protocol, &c.market] {
        let mark = if best.as_deref() == Some(route.name.as_str()) { "▸" } else { " " };
        let pays = route.receives_display.clone().unwrap_or_else(|| "—".into());
        println!("  {mark} {:<26} {}", route.name, ctx.out.bold(&pays));
        if let Some(why) = &route.unavailable {
            println!("      {}", ctx.out.dim(&format!("unavailable: {why}")));
            continue;
        }
        let steps: Vec<&str> = route.legs.iter().map(|l| l.label.as_str()).collect();
        if !steps.is_empty() {
            println!("      {}", ctx.out.dim(&format!("{} · {}", steps.join(" → "), route.wait)));
        }
        if !route.costs.is_empty() {
            println!("      {}", ctx.out.dim(&route.costs.join(" · ")));
        }
        for w in &route.warnings {
            println!("      {} {w}", ctx.out.yellow("!"));
        }
    }
    if let Some(bps) = c.market_advantage_bps {
        let (label, value) = if bps >= 0 { ("market route pays", bps) } else { ("protocol conversion pays", -bps) };
        println!("  {}", ctx.out.dim(&format!("{label} {}.{:02}% more right now", value / 100, (value % 100).abs())));
    }
}

/// Resolve a pair against the live market list.
async fn market_pair(s: &Session, pair: &str) -> Result<(wallet_core::markets::Pool, bool, String)> {
    let data = s.data_ctx()?;
    let (pools, _) = wallet_core::markets::all_markets(&data).await?;
    wallet_core::alerts::find_pair(&pools, pair, s.network.wquai.as_deref())
        .ok_or_else(|| CoreError::NotFound(format!("no market for `{pair}`; try a name from `quai-terminal markets`, like WQI/QUAI")))
}

pub async fn alert(ctx: &Ctx, cmd: AlertCmd) -> Result<()> {
    use wallet_core::alerts::{self, Alert, Rule};
    let s = ctx.session().await?;
    let network = s.network.id.clone();
    match cmd {
        AlertCmd::List => {
            let list = alerts::load(&s.app, &network);
            if ctx.out.json() {
                ctx.out.emit("alert list", &list);
                return Ok(());
            }
            if list.is_empty() {
                println!("no alerts · quai-terminal alert add WQI/QUAI --above 125");
                return Ok(());
            }
            let now = wallet_core::registry::now();
            let rows = list
                .iter()
                .map(|a| {
                    let fired = if a.fired == 0 { "never".to_string() } else { format!("{}s ago", now.saturating_sub(a.fired)) };
                    vec![a.id.to_string(), a.describe(), if a.active { "met".into() } else { "waiting".into() }, fired]
                })
                .collect::<Vec<_>>();
            ctx.out.table(&["id", "alert", "now", "last fired"], &rows);
            Ok(())
        }
        AlertCmd::Add { pair, above, below, moves } => {
            let rule = match (above, below, moves) {
                (Some(p), _, _) => Rule::Above { price: p },
                (_, Some(p), _) => Rule::Below { price: p },
                (_, _, Some(p)) => Rule::Moves { pct: p },
                _ => return Err(CoreError::Invalid("say when: --above, --below or --moves".into())),
            };
            let (pool, inverted, name) = market_pair(&s, &pair).await?;
            let alert = Alert { id: 0, pool: pool.address, name, inverted, rule, active: false, fired: 0 };
            let what = alert.describe();
            let id = alerts::add(&s.app, &network, alert)?;
            done(ctx, "alert add", serde_json::json!({"id": id, "alert": what}), &format!("alert {id} set: {what}"))
        }
        AlertCmd::Gas { below } => {
            let alert = Alert {
                id: 0,
                pool: String::new(),
                name: String::new(),
                inverted: false,
                rule: Rule::GasBelow { gwei: below },
                active: false,
                fired: 0,
            };
            let what = alert.describe();
            let id = alerts::add(&s.app, &network, alert)?;
            done(ctx, "alert gas", serde_json::json!({"id": id, "alert": what}), &format!("alert {id} set: {what}"))
        }
        AlertCmd::Rm { id } => {
            if !alerts::remove(&s.app, &network, id)? {
                return Err(CoreError::NotFound(format!("no alert {id}")));
            }
            done(ctx, "alert rm", serde_json::json!({"id": id}), &format!("alert {id} removed"))
        }
        AlertCmd::Check => {
            let fired = alerts::run(&s.data_ctx()?, ctx.config.features.trading).await?;
            if ctx.out.json() {
                ctx.out.emit("alert check", &fired.iter().map(|(t, b)| serde_json::json!({"alert": t, "detail": b})).collect::<Vec<_>>());
                return Ok(());
            }
            if fired.is_empty() {
                println!("nothing fired");
            }
            for (title, body) in fired {
                println!("{} {title} · {body}", ctx.out.green("●"));
            }
            Ok(())
        }
    }
}

pub async fn watch(ctx: &Ctx, cmd: WatchCmd) -> Result<()> {
    use wallet_core::alerts;
    let s = ctx.session().await?;
    let network = s.network.id.clone();
    match cmd {
        WatchCmd::List => {
            let list = alerts::watchlist(&s.app, &network);
            if ctx.out.json() {
                ctx.out.emit("watch list", &list);
                return Ok(());
            }
            if list.is_empty() {
                println!("nothing watched · quai-terminal watch toggle WQI/QUAI");
            }
            for pool in list {
                println!("{pool}");
            }
            Ok(())
        }
        WatchCmd::Toggle { pair } => {
            let (pool, _, name) = market_pair(&s, &pair).await?;
            let on = alerts::toggle_watch(&s.app, &network, &pool.address)?;
            let message = if on { format!("watching {name}") } else { format!("stopped watching {name}") };
            done(ctx, "watch toggle", serde_json::json!({"pool": pool.address, "watched": on}), &message)
        }
    }
}

/// One line of a batch file.
#[derive(Debug, PartialEq)]
pub struct BatchRow {
    pub line: usize,
    pub to: String,
    pub amount: String,
    /// `QUAI`, `QI`, or a token symbol or contract.
    pub asset: String,
}

/// `to,amount[,asset]` per line; blank lines, `#` comments and a `to,amount` header are skipped.
pub fn parse_batch(text: &str) -> Result<Vec<BatchRow>> {
    let mut rows = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cells: Vec<&str> = line.split(',').map(str::trim).collect();
        if rows.is_empty() && cells.first().is_some_and(|c| c.eq_ignore_ascii_case("to")) {
            continue;
        }
        let (to, amount) = match cells.as_slice() {
            [to, amount, ..] if !to.is_empty() && !amount.is_empty() => (*to, *amount),
            _ => return Err(CoreError::Invalid(format!("line {}: expected `to,amount[,asset]`", i + 1))),
        };
        if !amount.parse::<f64>().is_ok_and(|a| a > 0.0) {
            return Err(CoreError::Invalid(format!("line {}: `{amount}` is not an amount above zero", i + 1)));
        }
        let asset = cells.get(2).filter(|a| !a.is_empty()).map_or("QUAI".to_string(), |a| a.to_uppercase());
        rows.push(BatchRow { line: i + 1, to: to.to_string(), amount: amount.to_string(), asset });
    }
    if rows.is_empty() {
        return Err(CoreError::Invalid("the file has no sends in it".into()));
    }
    Ok(rows)
}

async fn send_batch(ctx: &Ctx, s: &mut Session, file: &std::path::Path, from: Option<&str>, dry_run: bool) -> Result<()> {
    let rows = parse_batch(&std::fs::read_to_string(file)?)?;
    // Prepare every one first: nothing is signed until the whole batch has been seen.
    let mut reviews = Vec::new();
    for row in &rows {
        let prepared = match row.asset.as_str() {
            "QUAI" => s.review_send_quai(from, &row.to, &row.amount, None).await,
            "QI" => s.review_send_qi(&row.to, &row.amount, None).await,
            token => s.review_send_token(from, token, &row.to, &row.amount, None).await,
        };
        match prepared {
            Ok(r) => reviews.push(r),
            Err(e) => {
                for r in &reviews {
                    let _ = s.discard(&r.op_id);
                }
                return Err(CoreError::Invalid(format!("line {}: {e}", row.line)));
            }
        }
    }
    let warned = reviews.iter().filter(|r| !r.warnings.is_empty()).count();
    // The warning a row leads with: a recipient check before anything else.
    let worst = |r: &wallet_core::tx::Review| -> String {
        r.warnings.iter().find(|w| wallet_core::recipient::is_recipient_warning(w)).or(r.warnings.first()).cloned().unwrap_or_default()
    };
    // Every warning of every row, on stderr so JSON output stays clean — even with `--json --yes`,
    // because a batch is where pasted addresses get looked at least.
    for (row, r) in rows.iter().zip(&reviews) {
        for w in &r.warnings {
            eprintln!("{} line {}: {w}", ctx.out.yellow("!"), row.line);
        }
    }
    if !ctx.out.json() || !ctx.global.yes {
        let table: Vec<Vec<String>> = rows
            .iter()
            .zip(&reviews)
            .map(|(row, r)| vec![row.line.to_string(), r.amount.clone(), r.to.clone(), r.max_fee.clone(), worst(r)])
            .collect();
        ctx.out.table(&["line", "amount", "to", "max fee", "warning"], &table);
        // What leaves, asset by asset, across the whole batch.
        let mut totals: Vec<(String, f64)> = Vec::new();
        for c in reviews.iter().flat_map(|r| &r.changes).filter(|c| c.direction == "out" || c.direction == "fee") {
            let v: f64 = c.amount.replace(',', "").trim_start_matches("≈ ").parse().unwrap_or(0.0);
            match totals.iter_mut().find(|(a, _)| *a == c.asset) {
                Some((_, t)) => *t += v,
                None => totals.push((c.asset.clone(), v)),
            }
        }
        let sum: Vec<String> = totals.iter().map(|(a, v)| format!("{v} {a}")).collect();
        eprintln!("{} sends · at most {} leaves, fees included", reviews.len(), sum.join(" + "));
        if warned > 0 {
            eprintln!("{} {warned} of them carry warnings, listed above; read them before approving", ctx.out.yellow("!"));
        }
    }
    if dry_run {
        for r in &reviews {
            let _ = s.discard(&r.op_id);
        }
        eprintln!("dry run: nothing signed");
        return Ok(());
    }
    if let Err(e) = prompt::confirm(&format!("Sign and broadcast all {}?", reviews.len()), "yes", ctx.global.yes) {
        for r in &reviews {
            let _ = s.discard(&r.op_id);
        }
        return Err(e);
    }
    let mut sent = Vec::new();
    let mut left = reviews.into_iter();
    for r in left.by_ref() {
        match s.commit(&r.op_id).await {
            Ok(submitted) => {
                if !ctx.out.json() {
                    ctx.print_submitted("send batch", &submitted);
                }
                sent.push(submitted);
            }
            Err(e) => {
                // Stop at the first failure: later sends may depend on its nonce or balance.
                for rest in left {
                    let _ = s.discard(&rest.op_id);
                }
                return Err(CoreError::Invalid(format!("stopped after {} of {}: {e}", sent.len(), rows.len())));
            }
        }
    }
    if ctx.out.json() {
        ctx.out.emit("send batch", &sent);
    }
    Ok(())
}

// ============================================================== arbitrary contracts

/// `contract inspect|read|call`: what an address is, and calling it through the ABI its own
/// bytecode names. The trust caveat is printed with every ABI, not only when something is wrong.
pub async fn contract(ctx: &Ctx, cmd: crate::args::ContractCmd) -> Result<()> {
    use crate::args::ContractCmd;
    match cmd {
        ContractCmd::Inspect { address, functions, source } => {
            let s = ctx.session().await?;
            let found = s.inspect_contract(&address).await?;
            let interface = found.metadata.as_ref().map(|m| m.interface()).transpose()?;
            let list = interface.as_ref().map(wallet_core::contracts::callables).unwrap_or_default();
            if ctx.out.json() {
                ctx.out.emit(
                    "contract inspect",
                    &json!({
                        "address": found.address,
                        "is_contract": found.is_contract(),
                        "code_len": found.code_len,
                        "code_hash": found.code_hash,
                        "code_proven": found.code_proven,
                        "solc": found.solc,
                        "abi_cid": found.metadata.as_ref().map(|m| m.cid.clone()),
                        "abi_checked_against_cid": found.metadata.as_ref().map(|m| m.checked_against_cid),
                        "name": found.metadata.as_ref().map(|m| m.name.clone()),
                        "compiler": found.metadata.as_ref().map(|m| m.compiler.clone()),
                        "verified": found.verified,
                        "undeclared_selectors": found.undeclared,
                        "error": found.metadata_error,
                        "functions": list.iter().map(|c| json!({"signature": c.signature, "payable": c.payable, "read_only": c.read_only})).collect::<Vec<_>>(),
                        "source": source.then(|| found.metadata.as_ref().and_then(|m| m.source.clone())).flatten(),
                    }),
                );
                return Ok(());
            }
            if !found.is_contract() {
                println!("{} is a plain account — no code here.", ctx.out.bold(&found.address));
                return Ok(());
            }
            let name = found.metadata.as_ref().map(|m| m.name.as_str()).unwrap_or("contract");
            println!("{} {}", ctx.out.bold(name), ctx.out.dim(&found.address));
            println!("  code        {} bytes · {}", found.code_len, found.code_hash);
            if let Some(confirmed) = &found.code_proven {
                println!("              {}", ctx.out.green(&format!("proven to be the chain's code here, {confirmed}")));
            }
            if let Some(v) = &found.solc {
                println!("  compiler    solc {v}");
            }
            match (&found.metadata, &found.metadata_error) {
                (Some(m), _) => {
                    let provenance = if m.checked_against_cid {
                        ctx.out.green("checked against the CID in the bytecode")
                    } else {
                        ctx.out.yellow("served by the gateway unchecked — too large to check against its CID")
                    };
                    println!("  ABI         {} ({provenance})", m.cid);
                    println!("  built as    {} in {}", m.name, m.source_path);
                }
                (None, Some(e)) => println!("  ABI         {}", ctx.out.yellow(e)),
                (None, None) => {}
            }
            match found.verified {
                Some(true) => println!("  explorer    {}", ctx.out.green("recompiled and matched against this bytecode")),
                Some(false) => println!("  explorer    {}", ctx.out.yellow("not verified")),
                None => println!("  explorer    {}", ctx.out.dim("not asked")),
            }
            if !found.undeclared.is_empty() {
                println!(
                    "  {}  {} the code dispatches on {} missing from the ABI: {}",
                    ctx.out.yellow("!"),
                    wallet_core::amount::count(found.undeclared.len(), "function"),
                    if found.undeclared.len() == 1 { "is" } else { "are" },
                    found.undeclared.join(" ")
                );
            }
            if found.metadata.is_some() {
                println!("\n  {}", ctx.out.dim("The ABI is what this contract says about itself. It is proof of what was published,"));
                println!("  {}", ctx.out.dim("not proof of what the deployed code does. Read the call data in every review."));
            }
            if functions && !list.is_empty() {
                println!("\n{}", ctx.out.bold("functions"));
                for c in &list {
                    let tag = if c.read_only {
                        ctx.out.dim("read")
                    } else if c.payable {
                        ctx.out.yellow("payable")
                    } else {
                        String::new()
                    };
                    println!("  {:<52} {tag}", c.signature);
                }
            }
            // The source is the one metadata field that is not sanitized on the way in, and it
            // comes from a gateway. Printed raw it is cursor control, colour and scroll-region
            // escapes — enough to rewrite the provenance lines above it.
            if source && let Some(text) = found.metadata.as_ref().and_then(|m| m.source.as_deref()) {
                println!("\n{}\n{}", ctx.out.bold("source"), wallet_core::explorer::clean_text(text));
            }
            Ok(())
        }
        ContractCmd::Read { address, function, args } => {
            let s = ctx.session().await?;
            let result = s.call_contract_read(&address, &function, &args).await?;
            if ctx.out.json() {
                ctx.out.emit("contract read", &json!({"signature": result.signature, "outputs": result.outputs}));
                return Ok(());
            }
            println!("{}", ctx.out.bold(&result.signature));
            for (name, value) in &result.outputs {
                println!("  {name:<20} {value}");
            }
            if result.outputs.is_empty() {
                println!("  {}", ctx.out.dim("(returned nothing)"));
            }
            Ok(())
        }
        ContractCmd::Call { address, function, args, value, account, fee } => {
            let mut s = ctx.unlocked().await?;
            let review =
                s.review_contract_call(account.as_deref(), &address, &function, &args, value.as_deref(), fee.max_fee.as_deref()).await?;
            let submitted = ctx.authorize(&mut s, review).await?;
            ctx.print_submitted("contract call", &submitted);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_policy_binds_exact_intent_and_raw_limits() {
        let network = NetworkProfile::builtins().remove(0);
        let mut op = wallet_core::appdb::Operation {
            id: "op".into(),
            network: network.id.clone(),
            kind: wallet_core::journal::OpKind::SendQuai,
            store: "quai".into(),
            account: "0x0011".into(),
            status: OpStatus::Prepared,
            tx_hash: None,
            asset: "QUAI".into(),
            amount: "100".into(),
            counterparty: "0x0022".into(),
            fee: "10".into(),
            detail: json!({}).into(),
            created: 100,
            updated: 100,
        };
        let mut review = Review {
            op_id: op.id.clone(),
            kind: op.kind.clone(),
            title: "Send".into(),
            network: network.name.clone(),
            from: "0x0011 (label)".into(),
            to: "0x0022".into(),
            asset: "QUAI".into(),
            amount: "display only".into(),
            amount_base: op.amount.clone(),
            max_fee: "rounded display".into(),
            fee_bps: None,
            fields: vec![wallet_core::tx::Field { label: "Signing digest".into(), value: "0xabc".into() }],
            coins: vec![],
            warnings: vec![],
            visuals: vec![],
            fee_over_policy: false,
            changes: vec![],
            risks: vec![],
            confirm: None,
        };
        let policy: AuthorizationPolicy = serde_json::from_value(json!({
            "version": 1, "wallet_id": "wallet", "network_id": network.id, "chain_id": network.chain_id,
            "genesis": network.genesis, "accounts": ["0x0011"], "kinds": ["send_quai"],
            "destinations": ["0x0022"], "signing_digests": ["0xabc"], "fee_store": "quai",
            "max_amount_base": "100", "max_fee_base": "10", "not_before": 100, "expires_at": 200, "max_review_age_secs": 30,
        }))
        .unwrap();
        assert!(policy.check("wallet", &network, &op, &review, 110).is_ok());
        op.fee = "11".into();
        assert!(policy.check("wallet", &network, &op, &review, 110).is_err(), "use exact fee, never rounded display");
        op.fee = "10".into();
        review.fields[0].value = "0xdef".into();
        assert!(policy.check("wallet", &network, &op, &review, 110).is_err(), "changed calldata changes signing digest");
        review.fields[0].value = "0xabc".into();
        assert!(policy.check("other wallet", &network, &op, &review, 110).is_err());
        assert!(policy.check("wallet", &network, &op, &review, 131).is_err());
        assert!(policy.check("wallet", &network, &op, &review, 200).is_err());
        review.to = "0x0033".into();
        assert!(policy.check("wallet", &network, &op, &review, 110).is_err());
    }

    #[test]
    fn a_batch_file_reads_rows_and_says_which_line_is_wrong() {
        let rows = parse_batch("to,amount,asset\n# payroll\n0x0011,5\nalice, 2.5 , wqi\n\n").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].to.as_str(), rows[0].amount.as_str(), rows[0].asset.as_str()), ("0x0011", "5", "QUAI"));
        assert_eq!((rows[1].line, rows[1].asset.as_str()), (4, "WQI"));
        assert!(parse_batch("0x0011,-1").unwrap_err().to_string().contains("line 1"));
        assert!(parse_batch("0x0011").unwrap_err().to_string().contains("line 1"));
        assert!(parse_batch("# nothing\n").is_err());
    }
}
