//! `quai-terminal`: CLI entry point. Without a command in a terminal, opens the TUI.

mod args;
mod commands;
mod daemon;
mod eco;
mod notify;
mod orders;
mod output;
mod prompt;
mod tui;

use args::{Cli, Command, DiagnosticsCmd, ThemeCmd};
use clap::{CommandFactory, Parser};
use commands::Ctx;
use wallet_core::CoreError;
use wallet_core::config::Feature;

fn command_name(cmd: &Option<Command>) -> &'static str {
    match cmd {
        None | Some(Command::Tui) => "tui",
        Some(Command::Wallet(_)) => "wallet",
        Some(Command::Account(_)) => "account",
        Some(Command::Balance(_)) => "balance",
        Some(Command::Receive(_)) => "receive",
        Some(Command::Send(_)) => "send",
        Some(Command::Token(_)) => "token",
        Some(Command::Contract(_)) => "contract",
        Some(Command::Portfolio(_)) => "portfolio",
        Some(Command::Price(_)) => "price",
        Some(Command::Swap(_)) => "swap",
        Some(Command::Pool(_)) => "pool",
        Some(Command::Farm(_)) => "farm",
        Some(Command::Nft(_)) => "nft",
        Some(Command::Market(_)) => "market",
        Some(Command::Markets(_)) => "markets",
        Some(Command::Board(_)) => "board",
        Some(Command::Data(_)) => "data",
        Some(Command::Qi(_)) => "qi",
        Some(Command::Mining(_)) => "mining",
        Some(Command::Payment(_)) => "payment",
        Some(Command::Convert(_)) => "convert",
        Some(Command::Wrap(_)) => "wrap",
        Some(Command::Tx(_)) => "tx",
        Some(Command::Plan(_)) => "plan",
        Some(Command::Order(_)) => "order",
        Some(Command::History(_)) => "history",
        Some(Command::Locks) => "locks",
        Some(Command::Alert(_)) => "alert",
        Some(Command::Watch(_)) => "watch",
        Some(Command::Contact(_)) => "contact",
        Some(Command::Network(_)) => "network",
        Some(Command::Config(_)) => "config",
        Some(Command::Daemon(_)) => "daemon",
        Some(Command::Status(_)) => "status",
        Some(Command::Notifications(_)) => "notifications",
        Some(Command::Theme(_)) => "theme",
        Some(Command::Diagnostics(_)) => "diagnostics",
        Some(Command::Completions { .. }) => "completions",
    }
}

/// The optional feature a command belongs to: it refuses to run while that feature is off.
fn required_feature(cmd: &Option<Command>) -> Option<Feature> {
    match cmd {
        Some(Command::Board(_)) => Some(Feature::Messaging),
        Some(
            Command::Swap(_)
            | Command::Markets(_)
            | Command::Pool(_)
            | Command::Farm(_)
            | Command::Watch(_)
            | Command::Order(_)
            | Command::Plan(_),
        ) => Some(Feature::Trading),
        Some(Command::Alert(args::AlertCmd::Add { .. })) => Some(Feature::Trading),
        Some(Command::Nft(_) | Command::Market(_)) => Some(Feature::Nfts),
        _ => None,
    }
}

async fn run(cli: Cli) -> Result<(), CoreError> {
    wallet_core::http::set_offline(cli.global.offline_data);
    let mut ctx = Ctx::new(cli.global)?;
    wallet_core::http::set_proxy(ctx.config.proxy.as_deref())?;
    // A hand-edited gateway that does not parse falls back to the public one rather than stopping
    // the wallet from opening; `config set` refuses such a value in the first place.
    for (content, configured) in [
        (wallet_core::ipfs::Content::Abi, ctx.config.abi_ipfs_gateway.as_deref()),
        (wallet_core::ipfs::Content::Media, ctx.config.ipfs_gateway.as_deref()),
    ] {
        if let Err(e) = wallet_core::ipfs::set_gateway(content, configured) {
            eprintln!("warning: {e}; using {} for {}", content.default_gateway(), content.label());
        }
    }
    if let Some(feature) = required_feature(&cli.command) {
        ctx.config.features.require(feature)?;
    }
    match cli.command {
        None => {
            if prompt::interactive() && std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                tui::run(ctx).await
            } else {
                Cli::command().print_help().ok();
                println!();
                Ok(())
            }
        }
        Some(Command::Tui) => tui::run(ctx).await,
        Some(Command::Wallet(c)) => commands::wallet(&mut ctx, c).await,
        Some(Command::Account(c)) => commands::account(&ctx, c).await,
        Some(Command::Balance(a)) => commands::balance(&ctx, a).await,
        Some(Command::Receive(a)) => commands::receive(&ctx, a).await,
        Some(Command::Send(c)) => commands::send(&ctx, c).await,
        Some(Command::Token(args::TokenCmd::Discover { import })) => eco::token_discover(&mut ctx, import).await,
        Some(Command::Token(c)) => commands::token(&ctx, c).await,
        Some(Command::Contract(c)) => commands::contract(&ctx, c).await,
        Some(Command::Portfolio(a)) => eco::portfolio(&mut ctx, a).await,
        Some(Command::Price(a)) => eco::price(&mut ctx, a).await,
        Some(Command::Swap(a)) => eco::swap(&ctx, a).await,
        Some(Command::Pool(c)) => eco::pool(&ctx, c).await,
        Some(Command::Farm(c)) => eco::farm(&ctx, c).await,
        Some(Command::Nft(c)) => eco::nft(&mut ctx, c).await,
        Some(Command::Market(c)) => eco::market(&mut ctx, c).await,
        Some(Command::Markets(a)) => eco::markets(&ctx, a).await,
        Some(Command::Board(c)) => eco::board(&ctx, c).await,
        Some(Command::Data(c)) => eco::data(&ctx, c).await,
        Some(Command::Qi(c)) => commands::qi_cmd(&ctx, c).await,
        Some(Command::Mining(c)) => commands::mining(&ctx, c).await,
        Some(Command::Payment(c)) => commands::payment(&ctx, c).await,
        Some(Command::Convert(c)) => commands::convert(&ctx, c).await,
        Some(Command::Wrap(c)) => commands::wrap(&ctx, c).await,
        Some(Command::Tx(c)) => commands::tx(&ctx, c).await,
        Some(Command::Plan(c)) => eco::plan(&ctx, c).await,
        Some(Command::Order(c)) => orders::run(&mut ctx, c).await,
        Some(Command::History(a)) => commands::history(&ctx, a).await,
        Some(Command::Locks) => commands::locks(&ctx).await,
        Some(Command::Alert(c)) => commands::alert(&ctx, c).await,
        Some(Command::Watch(c)) => commands::watch(&ctx, c).await,
        Some(Command::Contact(c)) => commands::contact(&ctx, c).await,
        Some(Command::Network(c)) => commands::network_cmd(&mut ctx, c).await,
        Some(Command::Config(c)) => commands::config_cmd(&mut ctx, c).await,
        Some(Command::Daemon(args::DaemonCmd::Run { interval, locked, detached })) => daemon::run(&ctx, interval, locked, detached).await,
        Some(Command::Daemon(args::DaemonCmd::Start { interval })) => daemon::start(&ctx, interval),
        Some(Command::Daemon(args::DaemonCmd::Stop)) => daemon::stop(&ctx),
        Some(Command::Daemon(args::DaemonCmd::Status)) => daemon::status(&ctx),
        Some(Command::Daemon(args::DaemonCmd::Unlock { all })) => daemon::unlock(&ctx, all).await,
        Some(Command::Daemon(args::DaemonCmd::Lock)) => daemon::lock(&ctx),
        Some(Command::Daemon(args::DaemonCmd::Unit)) => {
            print!("{}", daemon::unit(&ctx));
            Ok(())
        }
        Some(Command::Status(a)) => commands::status(&ctx, a).await,
        Some(Command::Notifications(a)) => commands::notifications(&ctx, a).await,
        Some(Command::Theme(ThemeCmd::List)) => tui::theme::list_cmd(&ctx),
        Some(Command::Theme(ThemeCmd::Show)) => tui::theme::show_cmd(&ctx),
        Some(Command::Theme(ThemeCmd::Preview { name })) => tui::theme::preview_cmd(&ctx, name.as_deref()),
        Some(Command::Diagnostics(DiagnosticsCmd::Terminal)) => tui::terminal::diagnostics_cmd(&ctx),
        Some(Command::Diagnostics(DiagnosticsCmd::Info)) => {
            let info = serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "sdk": "quai-sdk 0.1.0-alpha.1",
                "home": ctx.paths.root(),
                "config": ctx.paths.config_file(),
                "default_network": ctx.config.default_network,
                "default_wallet": ctx.config.default_wallet,
                "wallets": ctx.registry.list().map(|w| w.len()).unwrap_or(0),
                "insecure_dev_kdf": ctx.registry.insecure_kdf(),
            });
            if ctx.out.json() {
                ctx.out.emit("diagnostics info", &info);
            } else {
                println!("{}", serde_json::to_string_pretty(&info).unwrap_or_default());
            }
            Ok(())
        }
        Some(Command::Completions { shell }) => {
            clap_complete::generate(shell, &mut Cli::command(), "quai-terminal", &mut std::io::stdout());
            Ok(())
        }
    }
}

fn main() {
    // The origin every `startup.*` mark is measured from: taken before argument parsing, so a
    // launch measurement includes everything the user waited through.
    wallet_core::diag::started();
    // Before anything reads a password: nothing else running as this user may read our memory.
    daemon::protect_memory();
    // `quai-terminal ... | head` closes stdout early; exit quietly instead of panicking.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let text = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or("");
        if text.contains("Broken pipe") {
            std::process::exit(0);
        }
        tui::restore_terminal();
        default_hook(info);
    }));
    let mut cli = Cli::parse();
    if cli.global.json {
        cli.global.output = args::Output::Json;
    }
    let name = command_name(&cli.command);
    let out =
        output::Out { format: cli.global.output, color: !cli.global.no_color && std::io::IsTerminal::is_terminal(&std::io::stderr()) };
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start runtime: {e}");
            std::process::exit(9);
        }
    };
    let result = runtime.block_on(run(cli));
    if let Err(err) = result {
        out.error(name, &err);
        std::process::exit(err.exit_code());
    }
}
