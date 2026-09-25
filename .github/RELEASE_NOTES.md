Quai Terminal is a keyboard-first CLI and TUI wallet for Quai Network.

**This is an alpha.** It is published to be tried, not to be relied on. Read "Before you put funds
in it" below before doing anything with real money.

## What's new in 0.1.0-alpha.9

This release rebuilds how the wallet is put together. Most of it is underneath; what you will see:

- **Your keys live in the background daemon, not the terminal.** The terminal is now a client of
  the daemon: your password is checked there and the unlocked keys are held there, for that terminal
  alone. If the daemon stops, the terminal locks at once and says why. `--standalone` keeps
  everything in the terminal, as before.
- **Simple and Pro.** A new install starts in Simple: home, send and receive, one exchange,
  activity, NFTs and contacts. Pro adds markets, pools, launches, orders, PnL, the NFT marketplace
  and the network pages. An existing install stays Pro. Switch in Settings › Mode, or with `:pro`.
- **Every review is checked against the transaction it signs.** The wallet decodes the bytes it is
  about to sign and refuses when they do not say what the review says. Risky reviews (an unknown
  contract, an unlimited approval, a first payment to an address, half or more of an account) ask
  you to type a few short words. Calls to contracts the wallet does not know are allowed, with a
  review of the raw call.
- **Choose the account that acts.** `@` picks it, from anywhere; every card, form and review says
  which account it spends from.
- **One Exchange.** Swap, Convert and Wrap are one screen, and a multi-step trade (approve, wrap,
  swap) is one plan, walked the same way from the terminal and the command line. Rejecting a review
  brings back the form as you typed it.
- **Pictures are decoded in a separate, sandboxed process** that holds no keys.
- **Private messages, off by default.** Messages encrypted to one person with weekly keys, sent from
  a messaging account of their own. They stay off until you turn them on, and their cryptography has
  not yet had an independent review.
- **People** puts contacts and the payment channels to them on one screen; with messaging on, `5`
  opens the inbox.
- **Speed.** Measured against the previous build on the same machine: nothing is slower, and
  unlocking is about 9% faster.

## Download

The quickest way, on Linux or macOS, installs the right file for your computer and checks it:

```sh
curl -fsSL https://raw.githubusercontent.com/mpoletiek/quai-terminal/main/install.sh | sh
```

Or download a file yourself:

| Platform | File |
| --- | --- |
| Mac, Apple Silicon (M1 and later) | `quai-terminal-<version>-macos-apple-silicon.tar.gz` |
| Mac, Intel | `quai-terminal-<version>-macos-intel.tar.gz` |
| Linux, x86-64 | `quai-terminal-<version>-linux-x86_64.tar.gz` |

`SHA256SUMS` covers every archive. Check it before unpacking:

```sh
shasum -a 256 -c SHA256SUMS --ignore-missing
```

## Running it on a Mac

These builds are **not signed or notarized**, so macOS quarantines them and the first run fails with
"cannot be opened because the developer cannot be verified". That is Gatekeeper refusing an unsigned
download, not the wallet failing. Clear the quarantine flag on the binary you just unpacked:

```sh
tar -xzf quai-terminal-<version>-macos-apple-silicon.tar.gz
cd quai-terminal-<version>-macos-apple-silicon
xattr -d com.apple.quarantine quai-terminal
./quai-terminal
```

Only do that for a file you fetched from this page and checked against `SHA256SUMS`.

## Terminal

Any terminal works. **Ghostty** and **kitty** get the most: inline pixel graphics, the kitty keyboard
protocol, and automatic light/dark theming from the terminal's own background. Check what yours
reports:

```sh
./quai-terminal diagnostics terminal
```

`graphics_tier` of `Pixels` means images render as images; `light_background` shows whether the
background was successfully negotiated with the terminal.

## Before you put funds in it

Read this part.

- **Only part of trading has run on Quai mainnet.** Swaps, buys on Quainance's launch-zone curves,
  adding and removing liquidity, staking, unstaking and harvesting have all confirmed there from
  this wallet. Curve sells, Hartii trades, trades on the revenue launcher's curves, exact-output and
  split swaps, and the automatic wrap before a trade have not: they are qualified by simulation against the deployed contracts and by execution on a
  disposable local chain. A limit order's swap is an ordinary swap once you approve its review,
  but no order has yet been run to completion on mainnet.
- **This version's own transactions have not yet run on mainnet.** 0.1.0-alpha.9 rebuilds how
  transactions are built, reviewed and signed. Its swaps (with their approvals), adding and removing
  liquidity, and QUAI → Qi conversion were run end to end on a disposable local chain, and its
  reviews checked read-only against the deployed mainnet contracts; the mainnet runs above were
  made with earlier versions. Start with small amounts.
- It holds real keys. Back up your recovery phrase before funding anything, and try it with a small
  amount first.
- `wallet watch` creates a watch-only wallet that cannot sign. That is the safe way to look around.

## Start

```sh
./quai-terminal            # the TUI
./quai-terminal --help     # the commands
```
