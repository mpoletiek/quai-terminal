Quai Terminal is a keyboard-first CLI and TUI wallet for Quai Network.

**This is an alpha.** It is published to be tried, not to be relied on. Read "Before you put funds
in it" below before doing anything with real money.

## What's new in 0.1.0-alpha.6

- **Your own node now serves every read, reviews included.** When a monitoring node is set
  (`network monitor mainnet URL`, or the onboarding's "Your own node"), balances, quotes and the
  numbers in a review all come from it once it proves it is on the same chain. Transactions are
  still sent only through the network's RPC. A review opens with a warning when your node is 3 or
  more blocks behind that RPC. **If you already had a monitoring node,** it now also answers your
  reviews; `--trust-execution` is no longer needed and does nothing. A node on the internet must
  use https.
- **Onboarding says what each choice does.** The privacy step asks the question it actually answers,
  whether explorer.qu.ai may look up your addresses, and states what each answer gets and costs.
  The connections step shows what goes where (reads, sends, address lookups, market data) as you fill
  it in. System › Data sources shows the same.
- **HartiiLabs markets priced like pools.** A curve is priced from the reserves it trades against,
  before the fee. A graduated curve shows the QUAI locked in its pool as TVL, with a 24h change, and
  refreshes every few seconds along with the pools.
- **Faster reviews.** Exchanges are quoted at once rather than one after another, and contract checks
  take two round trips instead of five (quai-sdk 0.1.0-alpha.12). A swap review through the public
  RPC went from 9 s to 5 s, and through a node of your own it takes under a second.
- **Charts keep up.** A pair's chart refreshes every 5 seconds (it was every 10), and the pairs
  beside the cursor load before you reach them.
- **Explorer links are clickable.** Ctrl+click opens an address or transaction in your browser, and
  alt+click copies it. The Markets header names the token's and the pool's contracts.
- **Accounts.** Import a private key into an open wallet (Accounts › space › i), and add more
  addresses to a watch-only wallet (`a`, or `account watch ADDRESS`).
- **Safer media.** NFT metadata and pictures are read from your IPFS gateway only as the immutable
  content they name, never an `/ipns/` name or anything else on that host.

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

- **Only part of trading has run on Quai mainnet.** Swaps, bonding-curve buys, adding and removing
  liquidity, staking, unstaking and harvesting have all confirmed there from this wallet. Curve
  sells, Hartii trades, exact-output and split swaps, and the automatic wrap before a trade have
  not: they are qualified by simulation against the deployed contracts and by execution on a
  disposable local chain. A limit order's swap is an ordinary swap once you approve its review,
  but no order has yet been run to completion on mainnet.
- It holds real keys. Back up your recovery phrase before funding anything, and try it with a small
  amount first.
- `wallet watch` creates a watch-only wallet that cannot sign. That is the safe way to look around.

## Start

```sh
./quai-terminal            # the TUI
./quai-terminal --help     # the commands
```
