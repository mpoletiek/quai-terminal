//! Command-line interface definition.

use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// Quai Terminal — self-custodial Quai and Qi wallet for the terminal.
///
/// Run without a command in a terminal to open the interactive TUI.
#[derive(Parser, Debug)]
#[command(name = "quai-terminal", version, about, long_about = None, propagate_version = true)]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Args, Debug, Clone)]
pub struct Global {
    /// Data directory (default: platform data dir, or $QUAI_TERMINAL_HOME).
    #[arg(long, global = true, env = "QUAI_TERMINAL_HOME")]
    pub home: Option<PathBuf>,
    /// Wallet name or id.
    #[arg(long, short = 'w', global = true)]
    pub wallet: Option<String>,
    /// Network id (mainnet, orchard, or a custom network).
    #[arg(long, short = 'n', global = true)]
    pub network: Option<String>,
    /// Output format.
    #[arg(long, short = 'o', global = true, value_enum, default_value_t = Output::Human)]
    pub output: Output,
    /// Same as `--output json`.
    #[arg(long, global = true)]
    pub json: bool,
    /// Read the wallet password from this file descriptor (Unix) instead of prompting.
    #[arg(long, global = true)]
    pub password_fd: Option<i32>,
    /// Authorize the reviewed transaction without an interactive prompt.
    #[arg(long, short = 'y', global = true)]
    pub yes: bool,
    /// Optional JSON allowlist of exact signing digests and bounded automation scopes.
    #[arg(long, global = true)]
    pub authorization_policy: Option<PathBuf>,
    /// The typed confirmation a risky review asks for (an unknown contract, an unlimited
    /// approval, a first payment, half or more of the account), for use with `--yes`.
    #[arg(long = "confirm", global = true, value_name = "WORDS")]
    pub confirm_words: Option<String>,
    /// With `--yes`: sign risky reviews without their typed confirmation. For scripts that have
    /// already decided; the review still names each risk.
    #[arg(long, global = true)]
    pub accept_risk: bool,
    /// Skip every third-party lookup (explorer, prices, images, marketplace) for this command.
    #[arg(long, global = true)]
    pub offline_data: bool,
    /// Disable colors (also when NO_COLOR is set to any non-empty value, `0` included: the
    /// no-color.org convention is presence, not truth).
    #[arg(long, global = true)]
    pub no_color: bool,
    /// The TUI in plain mode, for screen readers and the Linux console: no pictures, no block
    /// digits, no motion, ASCII marks. Every action also has a command (`--output json`).
    #[arg(long, global = true)]
    pub plain: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    Human,
    Json,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Create, import, back up and manage wallets.
    #[command(subcommand)]
    Wallet(WalletCmd),
    /// Quai accounts (addresses) in the selected wallet.
    #[command(subcommand)]
    Account(AccountCmd),
    /// Show balances for all accounts and Qi.
    Balance(BalanceArgs),
    /// Show a receive address or payment code.
    Receive(ReceiveArgs),
    /// Send QUAI, Qi or tokens.
    #[command(subcommand)]
    Send(SendCmd),
    /// ERC-20 tokens and allowances.
    #[command(subcommand)]
    Token(TokenCmd),
    /// Inspect and call a smart contract through the ABI it publishes itself.
    #[command(subcommand)]
    Contract(ContractCmd),
    /// Portfolio value: every holding with a USD price, total and 7-day history.
    Portfolio(PortfolioArgs),
    /// Trading performance in QUAI: cost, realized and unrealized PnL per token, from the trades
    /// this wallet made.
    Pnl(PnlArgs),
    /// USD prices for QUAI, Qi and tokens.
    Price(PriceArgs),
    /// Swap tokens through Quainance (`swap quote FROM TO AMOUNT`, `swap FROM TO --amount N`).
    Swap(SwapArgs),
    /// NFTs held by this wallet.
    #[command(subcommand)]
    Nft(NftCmd),
    /// NFT marketplace listings (Bazarr) and buying.
    #[command(subcommand)]
    Market(MarketCmd),
    /// DEX markets (Quainance): pairs, or one pair's price, 24h stats, candles and trades.
    Markets(MarketsArgs),
    /// Liquidity positions on Quainance: list, add, remove.
    #[command(subcommand)]
    Pool(PoolCmd),
    /// Gauge staking on Quainance: list, stake, unstake, harvest, exit, incentivize.
    #[command(subcommand)]
    Farm(FarmCmd),
    /// The on-chain message board: read a channel.
    #[command(subcommand)]
    Board(BoardCmd),
    /// Private messages: set up a messaging account, publish your weekly key, send and read.
    #[command(subcommand, visible_alias = "msg")]
    Message(MessageCmd),
    /// Third-party data sources: status and connection test.
    #[command(subcommand)]
    Data(DataCmd),
    /// Qi coins, scanning and consolidation.
    #[command(subcommand)]
    Qi(QiCmd),
    /// Qi mining (coinbase) addresses and imported Qi keys.
    #[command(subcommand)]
    Mining(MiningCmd),
    /// Payment codes, peers and the Pelagus mailbox.
    #[command(subcommand)]
    Payment(PaymentCmd),
    /// Convert between QUAI and Qi.
    #[command(subcommand)]
    Convert(ConvertCmd),
    /// Wrap Qi (WQI) and QUAI (WQUAI).
    #[command(subcommand)]
    Wrap(WrapCmd),
    /// Transactions sent from this wallet.
    #[command(subcommand)]
    Tx(TxCmd),
    /// Inspect, resume or cancel durable trading plans. Resuming requires fresh reviews.
    #[command(subcommand)]
    Plan(PlanCmd),
    /// Durable limit triggers and explicitly bounded client execution.
    Order(crate::orders::OrderArgs),
    /// Activity: sent operations and received payments.
    History(HistoryArgs),
    /// Time-locked balances (conversions, redemptions).
    Locks,
    /// Price, move and gas alerts (checked by the daemon, or the TUI when no daemon runs).
    #[command(subcommand)]
    Alert(AlertCmd),
    /// Market pairs pinned to the top of Markets.
    #[command(subcommand)]
    Watch(WatchCmd),
    /// Address book.
    #[command(subcommand)]
    Contact(ContactCmd),
    /// Networks and node health.
    #[command(subcommand)]
    Network(NetworkCmd),
    /// Preferences.
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Run background monitoring and notifications.
    #[command(subcommand)]
    Daemon(DaemonCmd),
    /// Public status for status bars (Waybar/Quickshell).
    Status(StatusArgs),
    /// Recent notifications.
    Notifications(NotificationsArgs),
    /// Themes (Omarchy colors.toml compatible).
    #[command(subcommand)]
    Theme(ThemeCmd),
    /// Diagnostics.
    #[command(subcommand)]
    Diagnostics(DiagnosticsCmd),
    /// Open the interactive terminal UI.
    Tui,
    /// Generate shell completions.
    Completions {
        /// Shell.
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand, Debug)]
pub enum AlertCmd {
    /// This wallet's alerts.
    List,
    /// Alert on a pair (`WQI/QUAI`, either way round, or a pool address).
    Add {
        pair: String,
        /// When the price reaches this or more.
        #[arg(long, group = "rule")]
        above: Option<f64>,
        /// When the price falls to this or less.
        #[arg(long, group = "rule")]
        below: Option<f64>,
        /// When the pair moves this many percent over 24 hours, either way.
        #[arg(long, group = "rule")]
        moves: Option<f64>,
    },
    /// Alert when the network's gas price falls to this many gwei.
    Gas {
        #[arg(long)]
        below: f64,
    },
    /// Remove an alert by its id.
    Rm { id: u64 },
    /// Check every alert now; fired ones print and join the notifications.
    Check,
}

#[derive(Subcommand, Debug)]
pub enum WatchCmd {
    /// Watched pairs.
    List,
    /// Watch a pair (`WQI/QUAI` or a pool address), or stop watching it.
    Toggle { pair: String },
}

#[derive(Subcommand, Debug)]
pub enum WalletCmd {
    /// Create a new wallet with a fresh recovery phrase (24 words by default).
    Create {
        /// Wallet name.
        #[arg(long)]
        name: String,
        /// Recovery phrase length.
        #[arg(long, default_value_t = 24)]
        words: usize,
        /// Wordlist language.
        #[arg(long, default_value = "english")]
        language: String,
        /// Also prompt for a BIP39 passphrase (a separate secret; losing it loses funds).
        #[arg(long)]
        passphrase: bool,
        /// Skip the recovery phrase verification quiz (not recommended).
        #[arg(long)]
        skip_verify: bool,
    },
    /// Import from a recovery phrase, private key or keystore file.
    Import {
        /// Wallet name.
        #[arg(long)]
        name: String,
        /// What to import.
        #[arg(long, value_enum, default_value_t = ImportKind::Mnemonic)]
        from: ImportKind,
        /// Keystore JSON file (for --from keystore).
        #[arg(long)]
        file: Option<PathBuf>,
        /// Accept a keystore whose password protection is below the minimum (scrypt N·r·p under
        /// 2^20, PBKDF2 under 100,000 rounds, or a salt under 16 bytes). The key is re-encrypted
        /// under this wallet's own password either way; only the file itself was weak.
        #[arg(long)]
        allow_weak_kdf: bool,
        /// Wordlist language (mnemonic).
        #[arg(long, default_value = "english")]
        language: String,
        /// Prompt for a BIP39 passphrase (mnemonic).
        #[arg(long)]
        passphrase: bool,
        /// Discover used Quai accounts after import on this network.
        #[arg(long)]
        discover: bool,
    },
    /// Create a watch-only wallet from public addresses.
    Watch {
        /// Wallet name.
        #[arg(long)]
        name: String,
        /// Addresses (Quai or Qi).
        #[arg(required = true)]
        addresses: Vec<String>,
    },
    /// List wallets.
    List,
    /// Show the selected wallet.
    Show,
    /// Rename the selected wallet.
    Rename { name: String },
    /// Delete the selected wallet from this computer.
    Delete {
        /// Confirm by typing the wallet name.
        #[arg(long)]
        confirm: Option<String>,
    },
    /// Change the vault password.
    ChangePassword,
    /// Reveal the recovery phrase (requires the password).
    ExportMnemonic,
    /// Reveal the private key for an address (requires the password).
    ExportKey { address: String },
    /// Export an encrypted Web3 v3 keystore for an address.
    ExportKeystore {
        address: String,
        /// Output file.
        #[arg(long)]
        out: PathBuf,
    },
    /// Import an additional private key (Quai or Qi) into the wallet.
    ImportKey {
        /// Label.
        #[arg(long, default_value = "Imported")]
        label: String,
    },
    /// Write an encrypted backup of the wallet (keys, custody state, contacts, history).
    Backup {
        /// Output file.
        out: PathBuf,
    },
    /// Restore a wallet from an encrypted backup.
    Restore { file: PathBuf },
    /// Decrypt and check a backup without restoring.
    VerifyBackup { file: PathBuf },
    /// Mark the recovery phrase as backed up after passing a verification quiz.
    VerifyPhrase,
    /// Set the default wallet.
    Use { name: String },
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportKind {
    Mnemonic,
    Key,
    Keystore,
}

#[derive(Subcommand, Debug)]
pub enum AccountCmd {
    /// List Quai accounts.
    List {
        /// Include archived accounts.
        #[arg(long)]
        all: bool,
    },
    /// Derive the next Quai account.
    Add {
        #[arg(long)]
        label: Option<String>,
    },
    /// Watch another address in a watch-only wallet (Quai or Qi).
    Watch {
        address: String,
        #[arg(long)]
        label: Option<String>,
    },
    /// Rename an account (selector: label, address or number).
    Rename { account: String, label: String },
    /// Hide an account from lists.
    Archive { account: String },
    /// Show an archived account again.
    Unarchive { account: String },
    /// Find previously used HD accounts on the network.
    Discover {
        #[arg(long, default_value_t = 5)]
        gap: u32,
    },
}

#[derive(Args, Debug)]
pub struct BalanceArgs {
    /// Skip the Qi refresh (use the last snapshot).
    #[arg(long)]
    pub no_refresh: bool,
    /// Include token balances for this account.
    #[arg(long)]
    pub tokens: bool,
}

#[derive(Args, Debug)]
pub struct ReceiveArgs {
    /// Asset to receive.
    #[arg(long, value_enum, default_value_t = Asset::Quai)]
    pub asset: Asset,
    /// Quai account.
    #[arg(long, short)]
    pub account: Option<String>,
    /// For Qi: show a fresh single-use address instead of the payment code.
    #[arg(long)]
    pub address: bool,
    /// Print a QR code.
    #[arg(long)]
    pub qr: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asset {
    Quai,
    Qi,
}

#[derive(Args, Debug)]
pub struct FeeArgs {
    /// Maximum fee (QUAI for account transactions, Qi for Qi transactions).
    #[arg(long)]
    pub max_fee: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum SendCmd {
    /// Send QUAI.
    Quai {
        /// Recipient address or contact.
        #[arg(long)]
        to: String,
        /// Amount in QUAI.
        #[arg(long)]
        amount: String,
        /// Source account.
        #[arg(long)]
        from: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Send Qi to a payment code, contact or single-output address.
    Qi {
        #[arg(long)]
        to: String,
        /// Amount in Qi.
        #[arg(long)]
        amount: String,
        #[command(flatten)]
        fee: FeeArgs,
        /// Also prepare the mailbox notification if the recipient was not notified.
        #[arg(long)]
        notify: bool,
    },
    /// Send an ERC-20 token.
    Token {
        /// Token symbol or contract address.
        token: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        amount: String,
        #[arg(long)]
        from: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Send to many recipients from a CSV file: `to,amount[,asset]` per line (asset: QUAI by
    /// default, a token symbol or contract; `#` comments and a header line are skipped). Every
    /// transaction is prepared and reviewed first, then one confirmation sends them in order.
    Batch {
        file: PathBuf,
        /// Source account for QUAI and token sends.
        #[arg(long)]
        from: Option<String>,
        /// Prepare and show everything, then discard it without signing.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum TokenCmd {
    /// List imported tokens.
    List,
    /// Import a token by contract address.
    Import { address: String },
    /// Hide/remove a token from lists.
    Remove { token: String },
    /// Token balances.
    Balance {
        #[arg(long, short)]
        account: Option<String>,
    },
    /// Find tokens this wallet holds using the explorer (`--import` adds them to the token list).
    Discover {
        #[arg(long)]
        import: bool,
    },
    /// Show an allowance.
    Allowance {
        token: String,
        spender: String,
        #[arg(long, short)]
        account: Option<String>,
    },
    /// Approve a spender (unlimited unless --amount).
    Approve {
        token: String,
        spender: String,
        #[arg(long)]
        amount: Option<String>,
        #[arg(long, short)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Revoke a spender's allowance.
    Revoke {
        token: String,
        spender: String,
        #[arg(long, short)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
}

#[derive(Args, Debug)]
pub struct PortfolioArgs {
    /// Use the last Qi snapshot instead of refreshing known addresses.
    #[arg(long)]
    pub no_refresh: bool,
    /// Print the 7-day value series.
    #[arg(long)]
    pub history: bool,
}

#[derive(Args, Debug)]
pub struct PnlArgs {
    /// Also list the trades behind it, newest first.
    #[arg(long)]
    pub trades: bool,
    /// How many trades `--trades` lists.
    #[arg(long, default_value_t = 30)]
    pub limit: usize,
}

#[derive(Args, Debug)]
pub struct PriceArgs {
    /// Assets: `quai`, `qi`, token symbols or contract addresses (default: quai and qi).
    pub assets: Vec<String>,
}

#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true)]
pub struct SwapArgs {
    #[command(subcommand)]
    pub cmd: Option<SwapCmd>,
    /// Token to pay (`QUAI`, symbol or contract address).
    pub from: Option<String>,
    /// Token to receive.
    pub to: Option<String>,
    /// Amount to pay.
    #[arg(long)]
    pub amount: Option<String>,
    /// Slippage in basis points (default from `config set swap_slippage_bps`).
    #[arg(long)]
    pub slippage: Option<u16>,
    /// Explicit minimum recipient output (human token units; atomic routes only).
    #[arg(long)]
    pub min_output: Option<String>,
    /// Maximum price impact in basis points (atomic routes only).
    #[arg(long)]
    pub max_impact_bps: Option<u16>,
    /// Deadline in minutes (default from `config set swap_deadline_minutes`).
    #[arg(long)]
    pub deadline: Option<u32>,
    /// Account.
    #[arg(long, short)]
    pub account: Option<String>,
    #[command(flatten)]
    pub fee: FeeArgs,
}

#[derive(Subcommand, Debug)]
pub enum SwapCmd {
    /// Compare a bounded split and optionally execute its separately reviewed allocations.
    Split {
        from: String,
        to: String,
        amount: String,
        #[arg(long)]
        quote: bool,
        #[arg(long, default_value_t = 20)]
        slices: u16,
        #[arg(long)]
        slippage: Option<u16>,
        #[arg(long)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Compare direct and cross-venue routes with estimated gas and explicit sequential risk.
    Alternatives {
        from: String,
        to: String,
        amount: String,
        #[arg(long)]
        slippage: Option<u16>,
        #[arg(long, short)]
        account: Option<String>,
    },
    /// Receive an exact output with a strict maximum input; each approval/action is reviewed.
    ExactOutput {
        from: String,
        to: String,
        output_amount: String,
        #[arg(long)]
        max_input: String,
        #[arg(long)]
        quote: bool,
        #[arg(long)]
        deadline: Option<u32>,
        #[arg(long, short)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Quote a swap: route, expected and minimum output, impact, approval.
    Quote {
        from: String,
        to: String,
        amount: String,
        #[arg(long)]
        slippage: Option<u16>,
        #[arg(long, short)]
        account: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum PlanCmd {
    List,
    Show {
        id: String,
    },
    Resume {
        id: String,
        /// Explicitly release a saved unsigned review and prepare it again with current state.
        #[arg(long)]
        discard_unsigned: bool,
    },
    /// Stop future steps; previously submitted transactions and allowances remain.
    Cancel {
        id: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum NftCmd {
    /// NFTs held (ownership verified on-chain).
    List {
        #[arg(long, short)]
        account: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// One NFT: metadata, traits, owner and listing.
    Show { contract: String, token_id: String },
    /// Transfer an NFT (ERC-721, or ERC-1155 with --quantity).
    Transfer {
        contract: String,
        token_id: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        quantity: Option<String>,
        #[arg(long, short)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
}

/// On-chain message board (the `messages` contract configured for the network).
#[derive(Subcommand, Debug)]
pub enum BoardCmd {
    /// Be notified when someone says something in a channel, or in a sealed conversation with
    /// `--dm` (a payment code or a contact who has one). Again stops.
    Subscribe {
        chat: String,
        #[arg(long)]
        dm: bool,
    },
    /// Chats that notify.
    Subscriptions,
    /// Pin a chat beside every screen of the TUI (`--clear` unpins).
    Pin {
        chat: Option<String>,
        #[arg(long)]
        dm: bool,
        #[arg(long)]
        clear: bool,
    },
    /// Check subscribed chats now; news prints and joins the notifications.
    News,
    /// Messages in a channel, oldest first.
    Read {
        /// Channel name, up to 32 bytes.
        channel: String,
        /// Blocks of history to read.
        #[arg(long, default_value_t = 720)]
        blocks: u64,
    },
    /// Channels with messages on the board, busiest first.
    Channels {
        /// Blocks of history to scan.
        #[arg(long, default_value_t = 720)]
        blocks: u64,
    },
    /// An old (v1/v2) sealed conversation with a payment-code peer, oldest first. Read-only:
    /// private messages are `message send` now.
    Inbox {
        /// Payment code, or a contact who has one.
        peer: String,
        /// Blocks of history to read.
        #[arg(long, default_value_t = 720)]
        blocks: u64,
    },
    /// Post a message from your messaging account. It is public and permanent.
    Post {
        /// Channel name, up to 32 bytes.
        channel: String,
        /// The message, up to 1024 bytes.
        text: String,
        #[command(flatten)]
        fee: FeeArgs,
    },
}

/// `message` subcommands.
#[derive(Subcommand, Debug)]
pub enum MessageCmd {
    /// Choose the messaging account (not your main one) and make this wallet's messaging identity.
    Setup {
        /// Account: label, number or address.
        account: String,
        /// Move to another account, or start over: the old keys and history are deleted and your
        /// contacts see a new identity.
        #[arg(long)]
        new_identity: bool,
    },
    /// Messaging account, fingerprint, and whether this week's key is published.
    Status,
    /// Move QUAI to the messaging account for its fees. This links the two accounts on chain.
    Fund {
        /// Amount of QUAI.
        amount: String,
        /// Account to send from (default: the first).
        #[arg(long)]
        from: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Publish this week's key. `send` does it for you when it is due.
    Keys {
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Send a private message to a messaging address or a contact. The text is typed at the
    /// prompt or piped on stdin, never given as an argument.
    Send {
        peer: String,
        /// Read the message from this file instead.
        #[arg(long)]
        text_file: Option<std::path::PathBuf>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Read the chain for new messages.
    Sync,
    /// Conversations, newest first.
    List,
    /// People who wrote first and are waiting to be accepted.
    Requests,
    /// One conversation, oldest first (syncs first).
    Read {
        peer: String,
        /// Leave it unread.
        #[arg(long)]
        keep_unread: bool,
    },
    /// Accept someone who wrote first.
    Accept {
        peer: String,
        /// Also add them to the address book under this name.
        #[arg(long)]
        name: Option<String>,
    },
    /// Drop everything from an address from now on.
    Block { peer: String },
    /// Stop blocking an address; its messages arrive as requests again.
    Unblock { peer: String },
    /// Show both fingerprints to compare with them in person or over another channel.
    Verify {
        peer: String,
        /// They matched: remember it.
        #[arg(long)]
        confirm: bool,
    },
    /// Accept a peer's new identity key after their identity changed.
    Trust { peer: String },
}

#[derive(Args, Debug)]
pub struct MarketsArgs {
    /// Pair as BASE/QUOTE (e.g. QUAI/USDT) or pool address; omit to list pairs.
    pub pair: Option<String>,
    /// Candle timeframe: 15m, 1h, 4h or 1d.
    #[arg(long, default_value = "1h")]
    pub timeframe: String,
    /// Recent trades to show.
    #[arg(long, default_value_t = 10)]
    pub trades: usize,
}

/// `pool` subcommands.
#[derive(Subcommand, Debug)]
pub enum PoolCmd {
    /// Your liquidity positions, wallet and staked together.
    List,
    /// Quainance's launch zone: tokens on their bonding curve, graduated and pooled.
    Launches,
    /// Where a token stands on its bonding curve.
    Curve {
        /// Launched token: symbol or contract address.
        token: String,
    },
    /// Preview a curve buy/sell without reserving funds. AMOUNT may be `max`.
    CurveQuote {
        token: String,
        amount: String,
        #[arg(long)]
        sell: bool,
        #[arg(long)]
        account: Option<String>,
        #[arg(long)]
        slippage: Option<u16>,
    },
    /// Buy a token on its bonding curve with QUAI.
    CurveBuy {
        token: String,
        /// QUAI to spend.
        #[arg(long)]
        amount: String,
        /// Slippage in basis points.
        #[arg(long)]
        slippage: Option<u16>,
        #[arg(long)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Sell a token back to its bonding curve (an exact approval first; the QUAI is credited to claim).
    CurveSell {
        token: String,
        /// Tokens to sell.
        #[arg(long)]
        amount: String,
        #[arg(long)]
        slippage: Option<u16>,
        #[arg(long)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Claim the QUAI a curve credited to you (sales, and overshoot past its target).
    CurveClaim {
        token: String,
        #[arg(long)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Add liquidity, sized by either token. The paired amount comes from the pool's own ratio.
    Add {
        /// Quote only: no unlock, signing, broadcast or reservation.
        #[arg(long)]
        quote: bool,
        /// Pair as BASE/QUOTE (e.g. WQI/WQUAI) or the pool address.
        pair: String,
        /// Amount to deposit, in the token named by `--token`.
        #[arg(long)]
        amount: String,
        /// Which side `--amount` is in (e.g. WQUAI). Defaults to the pool's first token.
        #[arg(long)]
        token: Option<String>,
        #[arg(long)]
        slippage: Option<u16>,
        #[arg(long)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Remove a percentage of a position.
    Remove {
        /// Quote only for the selected owner; no reservation.
        #[arg(long)]
        quote: bool,
        /// Pair as BASE/QUOTE or the pool address.
        pair: String,
        /// How much of the position to withdraw, 1 to 100.
        #[arg(long, default_value_t = 100)]
        percent: u8,
        #[arg(long)]
        slippage: Option<u16>,
        #[arg(long)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
}

/// `farm` subcommands.
#[derive(Subcommand, Debug)]
pub enum FarmCmd {
    /// Gauge pools: what they pay, what you have staked, and what is claimable.
    List,
    /// Stake LP into the gauge that lists the pair: the Quainance gauge, or a launch-zone gauge.
    Stake {
        pair: String,
        /// Exact verified gauge address when several gauges list this pair.
        #[arg(long)]
        gauge: Option<String>,
        /// LP to stake; omit for everything unstaked in the wallet.
        #[arg(long)]
        amount: Option<String>,
        #[arg(long)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Take LP back out of the gauge.
    Unstake {
        pair: String,
        /// Exact verified gauge address when several gauges list this pair.
        #[arg(long)]
        gauge: Option<String>,
        /// LP to unstake; omit for everything staked.
        #[arg(long)]
        amount: Option<String>,
        #[arg(long)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Claim rewards without unstaking.
    Harvest {
        pair: String,
        /// Exact verified gauge address when several gauges list this pair.
        #[arg(long)]
        gauge: Option<String>,
        #[arg(long)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Unstake everything and claim, in one transaction.
    Exit {
        pair: String,
        /// Exact verified gauge address when several gauges list this pair.
        #[arg(long)]
        gauge: Option<String>,
        #[arg(long)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Fund a pool's rewards for everyone staking in it. This gives the tokens away.
    Incentivize {
        pair: String,
        /// Reward token: WQUAI, WQI or USDT.
        #[arg(long, default_value = "WQUAI")]
        reward: String,
        #[arg(long)]
        amount: String,
        /// Days to stream the reward over.
        #[arg(long, default_value_t = 30)]
        days: u32,
        #[arg(long)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
}

/// Listing order for `market listings`.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListingOrder {
    /// Lowest price first.
    Cheapest,
    /// Highest price first.
    Priciest,
    /// Most recently listed first.
    Newest,
}

impl ListingOrder {
    /// The core order.
    pub fn core(self) -> wallet_core::market::ListingSort {
        match self {
            ListingOrder::Cheapest => wallet_core::market::ListingSort::Cheapest,
            ListingOrder::Priciest => wallet_core::market::ListingSort::Priciest,
            ListingOrder::Newest => wallet_core::market::ListingSort::Newest,
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum MarketCmd {
    /// Active listings, cheapest first.
    Listings {
        /// Only this collection (contract address).
        #[arg(long)]
        collection: Option<String>,
        /// Order.
        #[arg(long, value_enum, default_value_t = ListingOrder::Cheapest)]
        sort: ListingOrder,
        #[arg(long, default_value_t = 25)]
        limit: usize,
    },
    /// Collections directory (explorer).
    Collections {
        /// Search by name.
        query: Option<String>,
        #[arg(long, default_value_t = 25)]
        limit: usize,
    },
    /// Re-check a listing on-chain.
    Check { contract: String, token_id: String },
    /// List an item you hold for sale on Bazarr (a Zora ask), or change its price. The one-time
    /// marketplace approvals run first, each as its own review.
    Sell {
        contract: String,
        token_id: String,
        /// Price, e.g. 250.
        #[arg(long)]
        price: String,
        /// QUAI (default), WQI, WQUAI or USDT.
        #[arg(long, default_value = "QUAI")]
        currency: String,
        #[arg(long, short)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Cancel your listing of an item.
    Unlist {
        contract: String,
        token_id: String,
        #[arg(long, short)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Your active listings.
    Mine {
        #[arg(long, short)]
        account: Option<String>,
    },
    /// Buy a Zora ask (approval steps run first when the ask is priced in a token).
    Buy {
        contract: String,
        token_id: String,
        #[arg(long, short)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
}

#[derive(Subcommand, Debug)]
pub enum DataCmd {
    /// Switches, backend per network and request budgets.
    Status,
    /// Contact every configured service once.
    Test,
}

#[derive(Subcommand, Debug)]
pub enum QiCmd {
    /// Exit to backed-up external Qi addresses without generating change (supports imported keys).
    Sweep {
        /// Distinct fresh same-zone addresses, one per denomination output; repeat --to.
        #[arg(long = "to", required = true, num_args = 1..)]
        destinations: Vec<String>,
        /// Quote the exact fee/output shape without reserving, signing or broadcasting.
        #[arg(long)]
        quote: bool,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Qi balance buckets.
    Balance,
    /// List coins (UTXOs).
    Utxos,
    /// Refresh known addresses.
    Refresh,
    /// Gap-50 scan, or a deep scan up to a raw index.
    Scan {
        #[arg(long)]
        deep: Option<u32>,
    },
    /// Consolidate coins.
    Consolidate {
        /// Aggregate small denominations (must be first Qi tx in a block).
        #[arg(long)]
        aggregate: bool,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Allocate a fresh single-use Qi address.
    NewAddress {
        #[arg(long)]
        label: Option<String>,
    },
    /// List allocated Qi receive addresses.
    Addresses,
}

#[derive(Subcommand, Debug)]
pub enum MiningCmd {
    /// Generate a new Qi coinbase address.
    New {
        #[arg(long, default_value = "mining")]
        label: String,
    },
    /// List coinbase (mining) addresses and imported Qi keys.
    List,
    /// Label an address.
    Label { address: String, label: String },
    /// Import a Qi private key (e.g. an existing mining payout key).
    ImportKey {
        #[arg(long, default_value = "mining")]
        label: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum PaymentCmd {
    /// Show this wallet's payment code.
    Code {
        #[arg(long)]
        qr: bool,
    },
    /// List payment-channel peers.
    Peers,
    /// Add a peer and scan for payments from it.
    Add {
        code: String,
        /// Save as a contact with this name.
        #[arg(long)]
        name: Option<String>,
    },
    /// Scan a peer's channel (continue from a raw index for deep recovery).
    Scan {
        code: String,
        #[arg(long)]
        from: Option<u32>,
    },
    /// Read the whole Pelagus mailbox: rescan announced senders already registered, and probe
    /// the others. A sender with Qi waiting becomes an offer; none is registered on its own.
    Discover,
    /// Channel offers: announced senders with Qi waiting, not registered yet.
    Offers,
    /// Accept a channel offer: register the sender and scan the channel, so its Qi joins the
    /// wallet. Announcements are unauthenticated; accept only senders you expect.
    Accept { code: String },
    /// Decline a channel offer; the sender is not offered again.
    Decline { code: String },
    /// Find private payments without knowing the sender: mailbox discovery plus a rescan of
    /// every registered channel (the TUI and daemon run this automatically while unlocked).
    Sync,
    /// Announce your payment code to a peer through the mailbox.
    Notify {
        peer: String,
        #[arg(long)]
        from: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConvertCmd {
    /// Execute a resumable market conversion using native swaps and WQI.
    Market {
        #[arg(value_enum)]
        direction: Direction,
        amount: String,
        #[arg(long)]
        account: Option<String>,
        #[arg(long)]
        slippage: Option<u16>,
        #[arg(long)]
        deadline: Option<u32>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Current-fee exact-qit MAX amount for Qi→QUAI; no signing or reservations.
    MaxQuote {
        #[arg(long)]
        account: Option<String>,
        #[arg(long, default_value_t = 300)]
        slippage: u16,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Quote a conversion with batch-discount (refund risk) scenarios.
    Quote {
        #[arg(value_enum)]
        direction: Direction,
        amount: String,
    },
    /// Convert QUAI from an account into Qi.
    QuaiToQi {
        #[arg(long)]
        amount: String,
        /// Slippage in basis points (default: suggested from the quote).
        #[arg(long)]
        slippage: Option<u16>,
        #[arg(long)]
        from: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Convert Qi into QUAI in an account.
    QiToQuai {
        #[arg(long)]
        amount: String,
        #[arg(long)]
        slippage: Option<u16>,
        #[arg(long)]
        to: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    QuaiToQi,
    QiToQuai,
}

impl Direction {
    pub fn key(self) -> &'static str {
        match self {
            Direction::QuaiToQi => "quai_to_qi",
            Direction::QiToQuai => "qi_to_quai",
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum WrapCmd {
    /// Current-fee exact-qit MAX amount for wrapping; no signing or reservations.
    MaxQuote {
        #[arg(long)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Wrapped balances and unclaimed WQI.
    Status {
        #[arg(long, short)]
        account: Option<String>,
    },
    /// Wrap Qi into WQI backing for an account (step 1).
    Qi {
        #[arg(long)]
        amount: String,
        #[arg(long, short)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Claim WQI after the wrap settles (step 2).
    Claim {
        #[arg(long, short)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Redeem WQI to Qi (whole Qi amounts).
    UnwrapQi {
        #[arg(long)]
        amount: String,
        #[arg(long, short)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Wrap QUAI into WQUAI.
    Quai {
        #[arg(long)]
        amount: String,
        #[arg(long, short)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
    /// Unwrap WQUAI into QUAI.
    UnwrapQuai {
        #[arg(long)]
        amount: String,
        #[arg(long, short)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
}

#[derive(Subcommand, Debug)]
pub enum TxCmd {
    /// Recent operations.
    List {
        #[arg(long, default_value_t = 25)]
        limit: u32,
    },
    /// Show an operation (id prefix or tx hash).
    Show { id: String },
    /// Reconcile all open operations with the node.
    Track,
    /// Wait until an operation leaves the pending state.
    Wait {
        id: String,
        #[arg(long, default_value_t = 300)]
        timeout: u64,
    },
    /// Release the nonce a review reserved when the wallet closed before it was signed.
    Cancel {
        /// Operation id (prefix) or transaction hash.
        id: String,
    },
    /// Rebroadcast the exact signed bytes of an operation.
    Rebroadcast { id: String },
    /// Speed up a pending transaction with a higher fee (Qi pays it out of the transaction's own change).
    Speedup {
        id: String,
        /// Minimum fee increase in percent.
        #[arg(long, default_value_t = 20)]
        bump: u16,
    },
    /// Use a nonce left unused by a rejected review (0 QUAI self-transfer) so queued transactions can be mined.
    FillGap {
        /// Account (label, address or number).
        #[arg(long)]
        from: Option<String>,
    },
}

#[derive(Args, Debug)]
pub struct HistoryArgs {
    #[arg(long, default_value_t = 50)]
    pub limit: u32,
    /// Clear displayed history for this network (custody records are kept).
    #[arg(long)]
    pub clear: bool,
}

#[derive(Subcommand, Debug)]
pub enum ContactCmd {
    /// Add a contact.
    Add {
        name: String,
        #[arg(long)]
        address: Option<String>,
        #[arg(long)]
        payment_code: Option<String>,
        #[arg(long, default_value = "")]
        note: String,
    },
    /// Edit a contact (only the given fields change; `--clear-*` removes one).
    Edit {
        name: String,
        #[arg(long)]
        rename: Option<String>,
        #[arg(long)]
        address: Option<String>,
        #[arg(long)]
        payment_code: Option<String>,
        #[arg(long)]
        note: Option<String>,
        #[arg(long)]
        clear_address: bool,
        #[arg(long)]
        clear_payment_code: bool,
    },
    /// List contacts.
    List,
    /// Remove a contact.
    Remove { name: String },
}

#[derive(Subcommand, Debug)]
pub enum NetworkCmd {
    /// List networks.
    List,
    /// Node health for the selected network.
    Health,
    /// Add a custom network.
    Add {
        id: String,
        #[arg(long)]
        rpc: String,
        #[arg(long)]
        chain_id: u64,
        /// Trusted genesis hash.
        #[arg(long)]
        genesis: Option<String>,
        /// Fetch and trust the node's genesis (only for nodes you control).
        #[arg(long)]
        trust_node_genesis: bool,
        /// Treat --rpc as a gateway base that derives /cyprus1.
        #[arg(long)]
        pathing: bool,
        #[arg(long)]
        name: Option<String>,
        /// Node supports the v0.56 specialized Qi fee estimator.
        #[arg(long)]
        specialized_fees: bool,
        #[arg(long)]
        wqi: Option<String>,
        #[arg(long)]
        wquai: Option<String>,
        #[arg(long)]
        mailbox: Option<String>,
        /// On-chain message board (see the quai-messages project; no bytecode pin here).
        #[arg(long)]
        messages: Option<String>,
        #[arg(long)]
        explorer: Option<String>,
        /// Explorer API base URL (portfolio, NFTs, history).
        #[arg(long)]
        explorer_api: Option<String>,
        /// Explorer API flavor.
        #[arg(long, value_enum, default_value_t = ExplorerFlavor::Blockscout)]
        explorer_kind: ExplorerFlavor,
        /// UniswapV2 router for swaps (no bytecode pin on custom networks).
        #[arg(long)]
        router: Option<String>,
        /// UniswapV2 factory.
        #[arg(long)]
        factory: Option<String>,
        /// USDT token.
        #[arg(long)]
        usdt: Option<String>,
        /// Zora V3 Asks module.
        #[arg(long)]
        zora_asks: Option<String>,
        /// Zora V3 module manager.
        #[arg(long)]
        zora_manager: Option<String>,
        /// Zora V3 ERC-721 transfer helper.
        #[arg(long)]
        zora_erc721_helper: Option<String>,
        /// Zora V3 ERC-20 transfer helper.
        #[arg(long)]
        zora_erc20_helper: Option<String>,
        /// Listings indexer base URL.
        #[arg(long)]
        listings_indexer: Option<String>,
        /// Maximum gas price for default fee policies (wei).
        #[arg(long, default_value = "100000000000000")]
        max_gas_price: String,
        /// Maximum total account fee (QUAI).
        #[arg(long, default_value = "25")]
        max_total_fee: String,
    },
    /// Remove a custom network.
    Remove { id: String },
    /// Set the default network.
    Use { id: String },
    /// Set transport consent for financial reads and broadcasts on a network.
    Transport {
        id: String,
        /// Accept remote plaintext HTTP; omit this flag to require HTTPS again.
        #[arg(long)]
        allow_insecure: bool,
    },
    /// Show, set or clear a monitoring endpoint. Every blockchain read uses it once it reports the
    /// same chain; transactions are broadcast through the network RPC, and a review warns when
    /// the endpoint is 3 or more blocks behind it.
    Monitor {
        id: String,
        /// JSON-RPC URL; must report the same chain id and genesis.
        url: Option<String>,
        /// Treat the URL as a gateway base that derives /cyprus1.
        #[arg(long)]
        pathing: bool,
        /// No longer needed: a monitoring endpoint serves every read, reviews included. Accepted
        /// so existing scripts keep working.
        #[arg(long, requires = "url", conflicts_with = "clear", hide = true)]
        trust_execution: bool,
        /// Accept remote plaintext HTTP for execution on this network.
        #[arg(long, requires = "url", conflicts_with = "clear")]
        allow_insecure_rpc: bool,
        /// Remove the monitoring endpoint.
        #[arg(long, conflicts_with = "url")]
        clear: bool,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExplorerFlavor {
    /// explorer.qu.ai style `/api/...`.
    Quai,
    /// Blockscout v2 `/api/v2/...`.
    Blockscout,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCmd {
    /// Show configuration.
    Show,
    /// Set a value.
    Set { key: String, value: String },
}

#[derive(Subcommand, Debug)]
pub enum DaemonCmd {
    /// Start watching every wallet in the background (locked; `daemon unlock` hands it keys).
    Start {
        /// Poll interval in seconds.
        #[arg(long, default_value_t = 20)]
        interval: u64,
    },
    /// Stop the background daemon; the wallets it held unlocked are locked.
    Stop,
    /// Whether it runs, which wallets it watches, which it holds unlocked.
    Status,
    /// Give the running daemon wallet passwords (each wallet that can sign, or `-w NAME`), so it
    /// reads their sealed chats and private payments.
    Unlock {
        /// Ask for every wallet even when `-w` names one.
        #[arg(long)]
        all: bool,
    },
    /// Lock wallets in the running daemon (`-w NAME` for one, otherwise all).
    Lock,
    /// Run in the foreground: watch every wallet, notify and write status.
    Run {
        /// Poll interval in seconds.
        #[arg(long, default_value_t = 20)]
        interval: u64,
        /// Ask for no passwords (monitoring only until `daemon unlock`).
        #[arg(long)]
        locked: bool,
        /// Started in the background by `daemon start` or the TUI (ignores hang-ups).
        #[arg(long, hide = true)]
        detached: bool,
    },
    /// Print a systemd user unit for the daemon.
    Unit,
}

#[derive(Args, Debug)]
pub struct StatusArgs {
    #[arg(long, value_enum, default_value_t = StatusFormat::Human)]
    pub format: StatusFormat,
    /// Include balances (off by default for privacy).
    #[arg(long)]
    pub show_balances: bool,
    /// Query the node instead of reading the daemon's status file.
    #[arg(long)]
    pub live: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusFormat {
    Human,
    Json,
    Waybar,
}

#[derive(Args, Debug)]
pub struct NotificationsArgs {
    #[arg(long, default_value_t = 20)]
    pub limit: u32,
    /// Mark all as read.
    #[arg(long)]
    pub read: bool,
}

#[derive(Subcommand, Debug)]
pub enum ThemeCmd {
    /// List available themes.
    List,
    /// Show the resolved theme palette.
    Show,
    /// Print a sample of the theme.
    Preview { name: Option<String> },
}

#[derive(Subcommand, Debug)]
pub enum DiagnosticsCmd {
    /// Probe terminal capabilities (colors, graphics, keyboard protocol).
    Terminal,
    /// Version, paths and configuration summary.
    Info,
}

/// Inspect and call a contract the wallet was never taught about.
#[derive(clap::Subcommand, Debug)]
pub enum ContractCmd {
    /// What is at an address: whether it is a contract, the ABI its bytecode names, and how far
    /// that ABI can be trusted.
    Inspect {
        /// Contract address or contact name.
        address: String,
        /// Also print the functions it declares.
        #[arg(long)]
        functions: bool,
        /// Print the source its metadata carries.
        #[arg(long)]
        source: bool,
    },
    /// Call a read-only function. Nothing is signed and nothing is sent.
    Read {
        /// Contract address or contact name.
        address: String,
        /// Function name, or its full signature when the name is overloaded.
        function: String,
        /// Arguments, in the order the ABI declares them.
        args: Vec<String>,
    },
    /// Call a state-changing function. Goes through the usual review before anything is signed.
    Call {
        /// Contract address or contact name.
        address: String,
        /// Function name, or its full signature when the name is overloaded.
        function: String,
        /// Arguments, in the order the ABI declares them.
        args: Vec<String>,
        /// QUAI to send with the call (payable functions only).
        #[arg(long)]
        value: Option<String>,
        #[arg(long, short)]
        account: Option<String>,
        #[command(flatten)]
        fee: FeeArgs,
    },
}

#[cfg(test)]
mod trading_parser_tests {
    use super::*;
    #[test]
    fn exact_output_amount_does_not_collide_with_global_output_format() {
        let cli = Cli::try_parse_from([
            "quai-terminal",
            "--output",
            "json",
            "swap",
            "exact-output",
            "QUAI",
            "USDT",
            "2",
            "--max-input",
            "100",
            "--quote",
        ])
        .unwrap();
        assert_eq!(cli.global.output, Output::Json);
        let Some(Command::Swap(SwapArgs { cmd: Some(SwapCmd::ExactOutput { output_amount, max_input, quote, .. }), .. })) = cli.command
        else {
            panic!("exact-output command");
        };
        assert_eq!(output_amount, "2");
        assert_eq!(max_input, "100");
        assert!(quote);
    }
}
