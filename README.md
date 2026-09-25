# Quai Terminal

> **Alpha.** Published to be tried, not relied on. It holds real keys: back up your recovery phrase
> before funding anything, and start with an amount you are willing to lose.
>
> **Only part of trading has run on Quai mainnet.** Swaps, buys on Quainance's launch-zone curves,
> adding and removing liquidity, staking, unstaking and harvesting have all confirmed there from
> this wallet. Curve sells, Hartii trades, trades on the revenue launcher's curves, exact-output and
> split swaps, and the automatic wrap before a trade have not: they are qualified by simulation
> against the deployed contracts and by execution on a disposable local chain.
>
> **This version's own transactions have not yet run on mainnet.** Since 0.1.0-alpha.9 the wallet
> builds, reviews and signs transactions in a new way. Its swaps (with their approvals), adding and removing
> liquidity, and QUAI → Qi conversion ran end to end on a local chain, and its reviews were checked
> read-only against mainnet's contracts; the mainnet runs above were made with earlier versions.
> Start with small amounts.
>
> **It has been reviewed, and not every finding is closed.** Several reviews have been run against
> this code; some of what they raised is fixed and some is still open. Those reviews are not
> published here, so treat this as software with known unfinished edges rather than as audited.
>
> `wallet watch` makes a watch-only wallet that cannot sign, which is the safe way to look around.

A self-custodial desktop wallet for both Quai ledgers (QUAI and Qi) on Cyprus-1. It has two faces over one Rust engine:

- a keyboard-first **TUI** (`quai-terminal` with no arguments), and
- a scriptable **CLI** with JSON output and stable exit codes.

It is built on [`quai-sdk 0.1.0-alpha.14`](https://crates.io/crates/quai-sdk). Feature scope follows Pelagus. **Not included yet:** offline signing, browser/dApp integration, and cross-zone sends or accounts.

It also connects to the Quai ecosystem: a USD **portfolio** with token prices, icons and value history (explorer.qu.ai), **token swaps** through Quainance, and **NFTs** (gallery, transfers, and buying Bazarr listings).

> **Alpha software.** The SDK is alpha. Wrapper contracts, the payment mailbox, conversions, swaps and NFT purchases have been exercised end to end on a local dev chain; swap quotes, listing checks and purchase calls are also checked read-only against mainnet. Try small amounts first before using mainnet funds.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/mpoletiek/quai-terminal/main/install.sh | sh
```

This downloads the prebuilt binary for your computer from the newest release (Linux x86-64, or macOS
on Apple Silicon or Intel), checks it against that release's `SHA256SUMS`, and puts `quai-terminal` in
`~/.local/bin`. Nothing is compiled and nothing runs as root. If that directory is not on your `PATH`,
the installer prints the line to add. To pin a release, put `QUAI_TERMINAL_VERSION=v0.1.0-alpha.2`
before `sh`; `QUAI_TERMINAL_INSTALL_DIR` picks another directory. To read the script before running it,
download [`install.sh`](install.sh) and run `sh install.sh`.

To remove it:

```sh
curl -fsSL https://raw.githubusercontent.com/mpoletiek/quai-terminal/main/uninstall.sh | sh
```

This stops the background daemon and removes the binary. It never deletes your wallets: it prints
where their data is, and how to delete it once you have your recovery phrases.

## Build

```sh
cargo build --release          # binary: target/release/quai-terminal
cargo test                     # unit tests (vault, amounts, config, themes, TUI helpers)
```

You need Rust 1.97 (pinned in `rust-toolchain.toml`). Dependencies are optimized even in debug builds, because Argon2 key derivation and Qi address grinding are CPU heavy.

## Quick start

```sh
quai-terminal                     # opens the TUI; first run walks look, privacy, connections, wallet
quai-terminal wallet create --name main
quai-terminal wallet import --name old --from mnemonic     # prompts for the phrase
quai-terminal balance
quai-terminal receive --asset qi --qr
quai-terminal send quai --to 0x00… --amount 1.5
quai-terminal convert quote quai-to-qi 100
quai-terminal portfolio --history              # USD values, prices, 7-day history
quai-terminal swap quote QUAI USDT 100         # route, minimum out, price impact
quai-terminal swap WQI USDT --amount 50        # exact approval first when needed, then the swap
quai-terminal contract inspect 0x00… --functions  # what it is, and the ABI it publishes itself
quai-terminal contract read 0x00… balanceOf 0x00… # a node simulation: nothing signed, nothing sent
quai-terminal nft list                         # ownership re-checked on-chain
quai-terminal market listings                  # Bazarr, cheapest first
quai-terminal market buy 0x00… 42              # re-checks the ask on-chain before the review
quai-terminal pool list                        # liquidity positions, wallet and staked
quai-terminal pool add WQI/WQUAI --amount 50 --token WQUAI   # size by either side; the pool sets the other
quai-terminal farm list                        # gauge pools: emission, period end, claimable
quai-terminal farm stake WQI/WQUAI             # stakes everything unstaked
quai-terminal farm harvest WQI/WQUAI
```

Secrets are never taken from arguments or environment variables:

- The recovery phrase and private keys are read from a hidden prompt, or from piped stdin.
- The wallet password comes from a hidden prompt or `--password-fd N`.
- Every value-moving command prints a full review and asks for confirmation. `--yes` skips the prompt, never the review.
- Every review is decoded from the transaction's own bytes and refused if they do not match what it says. A risky review (an unknown contract, an unlimited approval, a first payment to an address, half or more of the account) asks you to type short words such as `pay 5b32`. With `--yes`, add `--confirm "pay 5b32"`, or `--accept-risk` for a script that has already decided.

Select a wallet with `-w NAME` and a network with `-n mainnet|orchard|<custom>`. Add `-o json` for machine output.

## TUI

The TUI has seven sections with sub-tabs, down the rail on the left:

| Key | Section | Sub-tabs (`[` / `]`) |
| --- | --- | --- |
| `1` | **Home** | Portfolio (total, 7-day value, the full holdings table, NFT strip, attention items, recent activity) · Qi coins (cash drawer, aggregate and sweep; Pro) · Accounts (with time-locked funds) |
| `2` | **Markets** (Pro) | Pairs (every Quainance market — main pools, graduated launches, tokens on their bonding curves — with candles and a live flow of every swap) · Launches (Quainance's launch zone: tokens on their bonding curve with where they stand on it, buy/sell/claim on the curve) · Pools (liquidity positions and gauge staking) |
| `3` | **Trade** | Exchange (Swap, Convert QUAI ↔ Qi and Wrap on one card) · Orders (limit orders; Pro) · PnL (Pro) |
| `4` | **NFTs** | Collected · Explore (collections; Pro) · Listings (Bazarr; Pro) |
| `5` | **People** | Contacts (with the payment channels to them beneath) · Board (on-chain messages; with messaging on, `5` opens its inbox) |
| `6` | **Activity** | All · Sends · Receipts · Trades · NFTs |
| `0` | **System** | Wallets (switch, create, import) · Network (node health, hashrate per algorithm, transactions and gas paid per hour; Pro) · Settings · Data sources (Pro) |

**Simple and Pro.** A new install starts in **Simple**: Home and Accounts, the Exchange, Collected NFTs, Contacts and messages, Activity, Wallets and Settings. **Pro** adds the trader's views (Markets, Pools, Launches, Orders, PnL), the NFT marketplace, the Qi coin list, Network and Data sources. Switch in System › Settings › Mode, with `:pro` / `:simple` in the command palette, or `quai-terminal config set mode pro`. A configuration from before this choice existed stays Pro. Simple's exchange speaks of QUAI and Qi: WQUAI and WQI are steps a route takes, offered to pick only when you hold them.

Every screen uses the same keys for the same things. `space` opens the actions for what is selected, with that screen's own letters (on Pools, `h` harvests), and `?` lists the keys for the screen you are on. The mouse works too: click, scroll and hover. The wallet has the mouse, so the terminal's own link handling doesn't see it: on an address or transaction hash, ctrl+click opens it in the explorer and alt+click copies it (hovering shows which). Hold shift to select text with the terminal instead.

| Key | Action |
| --- | --- |
| `[` `]` / `tab` | sub-tabs / move focus between panes (and card fields) |
| arrows or `h` `j` `k` `l` | move. The arrows always move; Settings › "Move with h j k l" turns the letters off |
| `home` / `G`, `ctrl-d` / `ctrl-u` | first / last row, page down / up |
| `g` then a letter | go straight to a screen (`g g`: first row) |
| `'` then a label | jump to a labeled row (coins, activity) |
| `enter` / `esc` / `backspace` | open the detail view for the focused row / back / the screen before |
| `space` | actions for the selected item |
| `:` or `ctrl-p` | command palette: every action, screen, contact, holding, market and word, recent choices first, and typed intents (`send alice 5 quai`, `swap 10 wqi to usdt`) that open the form or card filled in; `ctrl-y` copies the entry's CLI command |
| `s` / `r` / `t` | send / receive / trade (opens Swap with the focused token) |
| `b` / `S` | buy / sell |
| `c` / `w` | convert QUAI ↔ Qi / wrap |
| `a` / `e` / `x` | add / edit / remove |
| `y` / `Y` | copy the selected address, tx hash or payment code / copy its explorer or Bazarr link (system clipboard, or OSC 52 over SSH) |
| `/` / `,` / `.` / `f` | search / sort / view / flip |
| `R` / `ctrl-r` | reload / full refresh |
| `N` / `W` / `$` | notifications / wallets / hide or show the balance |
| `@` | the account that acts: cards, sends and trades start from it (`account use N` on the command line) |
| `ctrl-l` | lock |
| `?` | the rest of this screen's keys, and what its words mean (`g` for the whole glossary) |
| `q` | quit |

**Pictures.** Token icons tint each token's symbol and allocation bar (contrast-checked), activity rows carry a token or collection badge, Home shows a few NFT thumbnails beside the total (never added to it), and swap and NFT reviews show the token icons or the NFT thumbnail above the fields. Amounts are always text.

**Markets.** Markets › Pairs is a trading view of every Quainance market: the main exchange's pools, the launch AMM's pairs for tokens that graduated from their curve (marked `◈`), and tokens still on their bonding curve (marked `○`, with how far they have raised in place of TVL). Each shows price, 24h change and TVL; the selected pair's candles (15m, 1h, 4h, 1d with `T`) with a price axis and volume bars, in your system's local time (4h and daily candles start at local midnight); 24h high, low, volume and trades; your holdings of both tokens; and a trade tape that marks your own trades. `f` flips base and quote, `t` opens the swap card for the pair (on a curve, its buy form; `S` sells to the curve). Prices come from pool reserves (Sync events) and the tape from Swap events: explorer.qu.ai pool logs on mainnet, `quai_getLogs` on the node elsewhere. A curve has no pool, so its chart and tape come from its trades in Quainance's launch index, market data only. `quai-terminal markets [PAIR] --timeframe 1h` prints the same.

**Signing sequences.** A swap that needs an approval, and an NFT purchase that needs marketplace and token approvals, run as one sequence: each step is still its own review, and the next review opens by itself once the previous transaction confirms, on whatever screen you are on (Home shows the progress). Rejecting any review, a failed step or a preparation error (such as too little balance, checked before any approval) ends the sequence. When wrapped Qi settles, the claim review opens for your signature; rejecting it stops the prompt until more backing arrives.

**Two markets between QUAI and Qi.** The Convert card (Trade › Exchange) quotes both at once for the amount you type: the **protocol conversion** (one transaction at the controller's rate, subject to the block's conversion-flow discount, output locked by the protocol for weeks) and the **market route** through Quainance (wrap → swap → unwrap: several transactions, LP fee, price impact and slippage, spendable in minutes). `r` picks the route; the better-paying one is marked. The market route runs as a signing sequence, each step its own review, and redeems exactly what its swap produced (redemptions are whole Qi; the remainder stays wrapped). `quai-terminal convert quote <direction> <amount>` prints the same comparison.

**Paying a WQUAI pool from QUAI.** A swap that needs WQUAI you do not hold wraps the missing amount first, and a swap that pays out WQUAI offers to redeem it for QUAI afterwards — still one review per transaction.

**Exchange cards.** Swap, Convert and Wrap share one card: pay and receive rows, available balance, `f` to flip, `/` to pick a token (icons, balances, holders, `✓`/`⚠` trust markers and lookalike warnings) and a live quote that updates as you type. A card's inputs take keys only while focused (`tab` or `enter` focuses, `esc` leaves), so number keys keep switching sections. Token inputs show a step indicator (`1 approve exact 50 WQI → 2 swap`); every step is its own review.

**Images.** Token icons and NFT thumbnails come through the explorer media proxy, are decoded in a background worker with size and pixel limits (remote SVG with every external reference disabled), and are cached locally. They render as kitty bitmaps, as half-block pictures in other terminals, or as monogram badges. Images never carry meaning on their own; System › Data sources turns them off.

**Reviews.** Every transaction opens a review. It starts with what the transaction does to your balances, asset by asset (what leaves, what arrives with its slippage floor, and the most the fee can be). A send also checks where the money is going: a first-time recipient gets a note, and an address that looks like one you know or that has only sent you dust gets a warning (address poisoning). `Reject` is focused by default, and `Approve & sign` stays disabled until you have scrolled to the end of the review. `esc` always rejects, and nothing is signed before approval. `y` copies a send as the equivalent CLI command.

**Swap card.** Before you type an amount it shows the pair's rate both ways, its 24h change and its pool, and draws the pair's hourly chart beneath the form. `%` steps through 25, 50, 75% and all of what MAX would pay.

**Limit orders.** Quai has no order book, so a limit order here is the wallet waiting on your behalf: from a quoted swap, `space` then `o` sets a target (`+5%` for 5% more back than now, or an amount to receive) and an expiry. Nothing is posted on-chain. Active orders are re-checked every 30 seconds, by the open terminal or, when it is closed, the background daemon. When one is reachable you are told once (on screen, on the desktop and in Notifications), and Trade › Orders prepares a fresh review for you to sign. Neither ever signs by itself. The order guarantees the target less your slippage allowance. `quai-terminal order create quai usdt 1 --target +5%`, `order observe` and `order run` do the same from the shell.

**Watchlist and alerts.** In Markets, `w` pins a pair to the top and `A` sets an alert from its current price: above a price, below it, or a move of some percent in 24 hours. Gas alerts come from the CLI (`alert gas --below 30000`). An alert fires once when its line is crossed and re-arms when it crosses back. The daemon checks alerts every poll, even when locked; without a daemon the TUI checks them every minute. Fired alerts join the notifications and the desktop notice. `alert list/add/gas/rm/check`, `watch list/toggle`.

**Layouts.** Settings › Layout: `auto` (the trader layout from 200 columns), `standard`, `trader` (Markets beside the swap card from 140 columns, so the chart stays in view while you trade) or `focus` (no section sidebar).

**Private messages.** Messaging is off until you turn it on (System › Settings), and its cryptography has not yet had an independent review. People › Board lists private conversations under the public channels. Each message is encrypted to one person (HPKE with weekly keys). On chain, anyone sees that your account sent something, when, and roughly how big, but not who it was for. How it works:

- **From the account in use.** Messages and board posts go from the account you have chosen (`@`, or `account use`), and it pays their fees. Each account is its own messaging identity with its own conversations: switch accounts and the Board shows that account's. Its row on the Board (*@label*) shows its balance, fingerprint and this week's key. A message ties the account's address, and what it holds, to the fact that it sent something.
- **Keys never leave this computer.** An account's keys are made the first time it publishes a key here. They are not in backups: a restore starts a new messaging identity with no history.
- **Weekly keys.** A new key is published in any week you use messaging (`K`, or automatically before your first message that week). Old keys are deleted two weeks after they are replaced, so a key stolen later cannot read what was sent before.
- **Anyone can write to you.** People you have not accepted wait under *requests*. They never notify, and their messages show only when you open them: `a` accepts, `B` blocks.
- **Fingerprints.** Compare them with `v`. If someone's identity key changes, sending stops until you accept the change (`T`).

Accepted conversations notify with "who and how many", never the text. CLI (from the account in use): `message status`, `keys`, `send PEER` (the text is typed or piped, never an argument), `read`, `list`, `requests`, `accept`, `block`, `unblock`, `verify`, `trust`, `sync`. Conversations from before this format (between payment codes) are no longer shown.

**Chat while you trade.** On the Board, `P` pins a channel or a private conversation beside every other screen (a column on the right when the terminal is wide, a strip along the bottom otherwise), and it has its own message box: Tab reaches it after the screen's last pane or card field (or `` ` `` jumps straight in), typing goes into the box, Enter posts it through the usual review, and Tab or Esc return to the screen with an unsent draft kept. `n` subscribes a channel: each new message from someone else becomes a notification saying who said what (`●` marks subscribed chats). Private conversations need no subscription. The daemon checks channels every poll, even when locked, and private conversations while unlocked; without a daemon the TUI checks every 30 seconds. `board subscribe NAME`, `board subscriptions`, `board pin`, `board news`.

**Wallets at a glance.** System › Wallets shows every wallet on this computer with its value, QUAI, Qi and largest holdings, and their total, without unlocking any of them. QUAI is read live from each wallet's public addresses; the rest is from when that wallet was last priced.

**Transaction timeline.** An operation's detail lists each stage it went through (prepared, signed, submitted, replaced, confirmed), when, and how long each took; an open one says what it is waiting for.

**Locking.** The wallet locks after the configured idle time (Settings → Auto-lock). While locked, the lock screen plays an Omarchy [ttfx](https://github.com/omacom/ttfx) effect; motion is reduced over SSH and can be turned off.

**Graphics.** QR codes are drawn as kitty-protocol bitmaps in kitty, Ghostty and WezTerm (not under tmux or SSH). Other terminals get Unicode half blocks; `graphics = "text"` disables QR codes.

**Selling NFTs.** Items you hold can be listed on Bazarr as Zora asks: `L` on an NFT (or `quai-terminal market sell CONTRACT ID --price 250 [--currency QUAI|WQI|WQUAI|USDT]`). The first listing asks for two one-time approvals (the Asks module, and the transfer helper for that collection), each its own review. `L` again changes the price, `X` (or `market unlist`) cancels, Listings › `m` (or `market mine`) shows yours. The wallet watches your listings on-chain and notifies you, and the daemon's desktop notifications, when one sells.

**Monitoring endpoint.** Your own node can serve every read: balances, quotes, markets, tracking, the board, and the reads that prepare a transaction. Set it in System › Network › `m`, or with `quai-terminal network monitor mainnet http://10.0.0.12:9200` (`--pathing` for a gateway base, `--clear` to remove). It must report that network's chain id and genesis, or it is not used. Transactions are still broadcast through the network's main RPC, because a monitoring node has no hashrate behind it. That RPC is also its witness: it must hold the same block that a review's state proofs are read at, and the review then says "confirmed by two nodes" (if the two disagree, the review is refused). The terminal, its background lanes, the daemon and the CLI all use it, and try it again every few minutes if it stops answering.

**Themes.** First run opens a theme showroom (live preview, type to filter), also available later from the command palette (`:` then "theme"). With `theme = "auto"` the wallet follows Omarchy's current `colors.toml` and reloads live when you switch themes; outside Omarchy it is Quai Dark, or Quai Light on a light terminal, and the terminal's own ANSI palette is a theme you can choose. Built in: Quai Red (brand red on black), Quai Dark/Light, High Contrast, Colorblind safe, Tokyo Night (Night, Storm, Moon, Day), Catppuccin (Mocha, Macchiato, Frappé, Latte), Gruvbox Dark/Light, Nord, Dracula, Rosé Pine (Main, Moon, Dawn), Kanagawa (Wave, Dragon), Everforest Dark/Light, One Dark, Solarized Dark/Light, Nightfox, Monokai Pro, Ayu Mirage and Flexoki Light, plus any Omarchy theme directory or `colors.toml` path. Every palette is contrast-checked (body text ≥ 4.5:1; state colors ≥ 3:1 on every surface). `QUAI_TERMINAL_THEME=nord quai-terminal` tries one for a session; `quai-terminal theme list` / `theme preview NAME` work from the shell.

**Accessibility.** States always carry a glyph and a word, not just a color. `--no-color` / `NO_COLOR` switches to monochrome, turns off block digits and reduces motion; Settings has motion, effects, big digits and a terminal bell. Everything in the TUI is also a CLI command with `--output json`.

**Delight, safely.** Confirmed payments and settled conversions get a small stamp in the corner (a bigger one for your very first receipt). Effects never draw over forms, reviews or amounts, and any key skips them.

## Features

- **Wallets:** create (24 words, verification quiz), import phrase, private key or Web3 keystore, watch-only. Also export phrase, key or keystore; change password; rename; delete. A private key can also join an existing wallet as another account (Accounts › `space` `i`, or `wallet import-key`); back the wallet up again afterwards, since its phrase does not cover that key. A watch-only wallet takes more addresses (Accounts › `a`, or `account watch ADDRESS`); a wallet with keys never watches an address it cannot sign for, so from one, Accounts › `space` `n` starts a new watch-only wallet.
- **Encrypted backups:** keys, custody state, contacts and history in one file. `wallet backup`, `verify-backup` and `restore`.
- **Contacts:** one payment code and any number of Quai accounts per person; payments by name go to the first account. Save an account or a code to someone already in your contacts (`contact save NAME ACCOUNT-OR-CODE`; in the TUI, `space` then `c` on Contacts, or `c` on a Board message). A replaced payment code, or an account or code taken from another contact, is said first and asks to continue. `contact forget NAME ACCOUNT` drops one account.
- **Quai accounts:** add, label, archive, discover used accounts; token import, balances, transfers, approvals, allowances, revocation.
- **Qi:** gap-50 and deep scans, coin view with denominations, fresh single-use and mining addresses, imported Qi keys, denomination-preserving and aggregate consolidation.
- **Private payments (BIP47 payment codes):** send to a code (with optional mailbox notification), peers and channel scans. While unlocked, the TUI and daemon read the Pelagus mailbox every ~90 s and rescan known channels for later payments (`payment sync` does it on demand). Mailbox announcements are unauthenticated, so a sender you have never added is not registered on its own: if Qi is waiting on its channel it becomes an **offer** in People › Contacts, in the channels pane (`payment offers`), which you accept (`enter`, `payment accept CODE`) or decline (`x`, `payment decline CODE`). You are notified of an offer once 1 Qi or more is waiting; smaller offers are listed quietly.
- **Conversions:** quotes with the node's discounted estimate (also for watch-only wallets), batch-discount (refund-risk) scenarios, suggested slippage and, on mainnet, the explorer's step-by-step preview (flow discount, kQuai, 10% floor); QUAI→Qi and Qi→QUAI; settlement and lock tracking.
- **Wrapping:** Qi → WQI in two steps, as in Pelagus: a Qi transaction to the WQI contract, then `claimDeposit` once the protocol backing settles (Home shows "ready to claim"). WQI → Qi redeems whole Qi to a fresh Qi address, where it arrives locked. QUAI ↔ WQUAI deposit and withdraw. On mainnet the WQI and WQUAI bytecode is checked against pinned hashes before any review. `wrap status/qi/claim/unwrap-qi/quai/unwrap-quai`.
- **Portfolio:** every holding with a USD price and value, allocation, 24h change, 7-day value history, token icons and trust markers (`✓` verified or curated, `⚠` unverified, `◈` protocol-derived Qi price, `◔` stale). Discovered token balances are re-read on-chain. NFTs are listed separately and never added to the total. `portfolio`, `price`, `token discover`.
- **Markets:** sort the pairs by depth (`L`) or by how far they moved in 24 hours (`M`), each cycling high, low, then back to the directory's order, always by the figures the rows show and from the top of the list; watched pairs stay at the top of every order.
- **Swaps:** Quainance's exchanges (the main one, the launch AMM and the revenue AMM that tokens graduate into) and a shortlist of QuaiSwap pools, each router and factory pinned by bytecode. A token with a bonding curve is quoted on the curve too, and traded there when it pays more. Best route directly or through WQUAI, WQI or USDT; a pair only the two exchanges together connect (a graduated token for USDT, say) is two swaps through a hub, each reviewed on its own, the second sized from what the first actually paid; minimum output from your slippage; price impact from pool reserves (warning above 2%, refused above 50%); pool liquidity (TVL and its age, with a warning below $1,000); a deadline; exact approvals only. The actual output is recorded from the receipt. `swap quote`, `swap`.
- **NFT market stats:** Explore shows each collection's floor, 7-day volume, sales, listings and holders, sorted by any of them (`S`), over a header with the market's own week; a collection page adds its sale history, with a price line and who paid what. Figures come from the marketplace indexer, and only QUAI-paid sales are added up, so a total is never a mixed-currency sum.
- **NFTs:** a collection page lists its active listings (`tab` to select, `b` to buy); holdings found through the explorer and verified on-chain (`ownerOf` / `balanceOf`), metadata and traits, transfers (ERC-721 and ERC-1155), the collections directory with floors, and Bazarr listings. Zora V3 asks can be bought in the wallet: the ask, the seller's ownership and approvals are re-checked on-chain, and token-priced asks add the module and token approval steps. Seaport listings open as a Bazarr link. Listings sort by price or age and filter by collection (`S`, `f`/`F`; `market listings --sort`), and prices in WQI, WQUAI or USDT are named. `nft list/show/transfer`, `market listings/collections/check/buy`.
- **Contracts the wallet was never taught about:** Quai requires contracts to be built with `bytecodeHash: "ipfs"`, so a deployed contract carries the CID of its own metadata in its runtime code. The wallet reads that CID out of the bytecode, fetches the standard-json through the ABI gateway, and **checks the bytes against the CID** — a metadata document that fits in one IPFS block can be verified by arithmetic rather than trusted, and the wallet says which of the two happened. From the ABI it lists the functions, types the arguments, simulates the call and reviews it. Typing a contract address into the send form says so before you send QUAI to something that cannot receive it; `^F` there opens the call form, where picking a function brings up its arguments. `contract inspect ADDR [--functions] [--source]`, `contract read ADDR FN ARGS…` (a node simulation: nothing signed, nothing sent), `contract call ADDR FN ARGS… [--value N]`. **What this does and does not prove:** the CID→bytes link is verified, but *which* CID a contract embeds is its author's choice, and metadata commits to source, not to deployed code. So an ABI here is a decoding aid, never an endorsement: every review carries that caveat, shows whether the explorer has recompiled the contract, names any function the code dispatches on that the ABI leaves out, and prints the exact call data that gets signed.
- **Feature switches:** messaging, trading and NFTs can each be turned off in System › Settings or with `config set features.<messaging|trading|nfts> off`. An off feature is hidden in the terminal, its commands refuse to run, and neither the terminal nor the daemon polls for it. Converting and wrapping are wallet operations and stay available with trading off, as do gas alerts. Messaging starts off; trading and NFTs start on.
- **Privacy:** anything that tells a third party which addresses are yours is opt-in. Onboarding asks one question about the explorer: *Private* (the default) never tells explorer.qu.ai your addresses; *Connected* lets it find tokens and NFTs sent to you, value history and pictures, and it then sees your addresses and IP. That choice does not change which node the wallet uses: balances, quotes and reviews come from your own node when you set one (the next onboarding step, or `network monitor`), otherwise from rpc.quai.network, which then sees the addresses it is asked about; transactions are always broadcast through rpc.quai.network. The connections step shows what goes where as you fill it in, and System › Data sources shows it afterwards. Each source stays a switch (System › Data sources, or `config set explorer_lookups true`). Pictures load only over HTTPS from the explorer's media proxy and the IPFS gateway, and no request follows a redirect to another host. `config set proxy socks5h://127.0.0.1:9050` sends every third-party lookup and node RPC through Tor and fails closed when the proxy is down; a node on this machine or the LAN is still reached directly. IPFS content comes through gateways you choose, and there are two: one for contract ABIs and one for everything else, because they want different nodes. `ipfs.qu.ai` is the default for both — it is the authority for Quai contract metadata, which is what `config set abi_ipfs_gateway` points at; `config set ipfs_gateway http://127.0.0.1:8080` puts images and NFT metadata on your own node (reached directly, not through the proxy), which is the bulk of the fetching and the most revealing. Both take `https://{cid}.ipfs.dweb.link` style templates for subdomain gateways and `default` to reset; each is tested before it is saved, and content whose CID pins its bytes is checked against it. `--offline-data` disables every third-party lookup for one command; `data status` and `data test` show backends, request budgets and errors.
- **Transactions:** journaled operations, confirmation and settlement tracking, rebroadcast, speed-up by replacement, incoming activity. A rejected review releases its nonce; the next account transaction reuses it automatically (`tx fill-gap` fills one explicitly, e.g. before a conversion).
- **Networks:** mainnet and Orchard built in; custom networks are pinned by chain id and genesis, with node health checks and fee caps. `network monitor <id> <url>` adds a monitoring endpoint (your own node, say) that every read goes to; it must report the same chain id and genesis, and broadcasts always go to the main RPC. Reviews read from it too, and a review warns first when it is 3 or more blocks behind the main RPC.
- **Scripting:** `--json` on any command (same as `--output json`). `send batch FILE.csv` sends to many recipients: `to,amount[,asset]` per line; it prepares and shows every send, with its warnings and what leaves in total, asks once, sends in order and stops at the first failure (`--dry-run` signs nothing).
- **Daemon:** there is nothing to set up. Opening the terminal starts a background daemon, and the terminal is its client: the wallet engine runs in the daemon, so your password is checked there and the unlocked keys are held there for that terminal alone, never in the terminal's own process (`--standalone` keeps the engine in the terminal instead). The daemon also reads markets, NFTs and prices for every open terminal, with one set of caches and request budgets between them, and watches every wallet on this computer (balances, activity, Qi, alerts, subscribed chats, desktop notifications, the status file). A wallet unlocked in the terminal is unlocked for that watch too, so its interval conversions and private messages keep going after you quit; Settings can turn that off. If the daemon stops, the terminal locks at once, says why and reconnects. Quitting leaves the daemon running and says so; `quai-terminal daemon stop` stops it. A daemon left over from an older build is replaced by the next terminal you open. The socket and its directory must be yours alone, every connection's peer must run as you (the kernel's `SO_PEERCRED`), and the terminal talks only to the daemon holding the lock; the password is sent once, never stored, and wiped on both sides. For the rest there are commands, none of them needed: `daemon status`, `start`, `stop`, `unlock` / `lock` (by hand, `-w NAME` for one wallet), `run` (the same loop in the foreground) and `unit` (a systemd user unit). Its log is `daemon.log` in the data directory. OSC 9/99 terminal notifications and `notify-send`; `status --format waybar` for Waybar.

## Data and security

- **Location:** data lives under the platform data directory, or under `--home` / `QUAI_TERMINAL_HOME`.
- **Layout:** each wallet has a vault, public metadata, and per-network SQLite stores. Stores are scoped by chain id and genesis, so data from one network can never be used on another.
- **Shared market cache:** `shared.sqlite` at the data root holds the market data that is the same for every wallet — prices, the pool directories, listings and their previews, candles, the DEX tape — so several wallets (and the daemon) fetch it once. It never holds anything that says which wallet asked: holdings, history, a wallet's NFTs and everything else address-linked stay in that wallet's own files. Delete it any time; it refills.
- **Vault encryption:** Argon2id (256 MiB, t=3, p=4 for new vaults; a re-seal never lowers a vault's cost) with XChaCha20-Poly1305. The file is written atomically with mode 0600.
- **Locking:** keys stay in memory only while the wallet is unlocked, in the daemon's engine (or the terminal's, with `--standalone`), and are zeroized on lock.
- **Pictures:** every image from the explorer, IPFS or a token list is decoded in a fresh helper process that holds no keys, with resource limits and, on Linux, a seccomp allowlist. The wallet takes back raw pixels only, checks their size and encodes them itself; a picture that crashes or stalls the helper shows a monogram.
- **Displayed data:** token names, symbols and other on-chain strings are treated as untrusted display text.
- **Reviews read the chain:** nothing a review asserts comes from a cache. Pinned contract bytecode, pair addresses, balances and allowances are read first-hand every time a review is prepared.
- **Third-party data:** explorer.qu.ai (mainnet) and orchard.quaiscan.io (Orchard) see the addresses you look up and your IP; the first mainnet use says so once. Requests go through per-host budgets (explorer.qu.ai 60/min, the Bazarr indexer 30/min), with 2 MB JSON and 8 MB image caps. Indexer data never reaches signing: reviews use node state, and contract calls carry a node-discovered access list shown in the review.

## Out of scope

Decided, and not planned for a later release:

- **Bridging.** Quainance's Symbiosis routes need a signature on Base or Ethereum. This wallet holds Quai keys only, and taking custody of EVM keys would change its threat model.
- **Launching tokens.** Launches can be browsed and traded on their bonding curves (Trade › Launches), but the wallet does not create them.
- **Private messages from unlinked addresses.** A private message is posted from the account in use, so it is linked to that account on chain. To keep messages apart from your holdings, message from an account kept for it (`account add`, then `@`); funding that account links the two. Posting from an address with no on-chain link to you would need gas that reaches it without leaving a link, and the wallet does not attempt that.

## Known limitations

- **Cyprus-1 only.** Cross-zone transfers are deferred.
- **Change outputs:** one Qi transaction can use at most 48 fresh change outputs (SDK address-search bounds). Very fragmented wallets should consolidate with `qi consolidate --aggregate`. Aggregation must be the first Qi transaction in a block, so it may be delayed.
- **Terminal background:** the light/dark background is probed once at startup (OSC 11). Omarchy theme changes reload live.
- **Payment discovery:** unknown-sender discovery needs the private keys, so it runs only while an unlocked TUI or `daemon run` is active. A payment from a sender who never posts a mailbox notification (and whom you haven't added) can't be found by any BIP47 wallet.
- **Prices:** QUAI and token prices come from explorer.qu.ai (QUAI from MEXC; Qi is protocol-derived) and are display estimates only. Qi and WQI can show different USD values. There are no prices on Orchard or custom networks.
- **Swaps:** Quainance's exchanges (the main one, the launch AMM and the revenue AMM) and a shortlist of QuaiSwap pools, whose other pairs are not used. A token with a Quainance or HartiiLabs bonding curve is quoted on its curve too, and the swap card trades there when it pays more or the exchanges cannot fill the amount; poop.fun curves are not supported. Each swap stays on one exchange, going direct, through one hub, or through two (WQUAI, WQI, USDT). Across the two exchanges a route is two transactions: the second swap's minimum is set when it is reviewed, so the route as a whole has no single minimum, and stopping after the first leaves you holding the hub token. Liquidity can only be added on the main exchange. Quainance runs on mainnet; custom networks can configure a router with `network add --router/--factory`.
- **Liquidity:** add, remove, stake and harvest LP on Quainance, and fund a pool's rewards for its stakers (`farm incentivize`). Reward APR is derived from the gauge's own emission rate and shows `—` rather than a guess when the reward token has no price or the stream has ended. The gauge has no emergency withdraw — `withdraw` is the only exit. Depositing into an *empty* pool is refused: the first deposit sets the price and is immediately arbitraged.
- **NFTs:** explorer.qu.ai returns NFT balances without ids, so ids come from replaying the address's transfers (up to 10,000). Listing creation and Seaport fills are not supported yet.
- **Value history** is QUAI balance history at today's price, with other holdings held constant.

## Repository layout

```
crates/quai-model      domain types: amounts, chain, errors, the operation journal
crates/quai-venues     the venue table, capabilities and pinned contracts (no node, no store)
crates/quai-feeds      untrusted inputs: HTTP, explorer, IPFS, pictures (decoded in a sandboxed helper)
crates/quai-messaging  the message board format and the private-message wire format
crates/wallet-vault    encrypted vault, atomic private file writes
crates/wallet-core     sessions, registry, custody, operations, tracking, backups, portfolio,
                       swaps, routing, liquidity, gauge staking, NFT market
crates/quai-engine     the engine: worker, plans, resources, the socket protocol the daemon serves
crates/quai-terminal   the binary: clap CLI, daemon, notifications, ratatui TUI
```
Each crate depends only on those below it; a test enforces that.
