Quai Terminal is a keyboard-first CLI and TUI wallet for Quai Network.

**This is an alpha.** It is published to be tried, not to be relied on. Read "Before you put funds
in it" below before doing anything with real money.

## What's new in 0.1.0-alpha.7

- **Reviews check the chain's own state, not only a node's answers** (quai-sdk 0.1.0-alpha.14).
  The contracts a review relies on are proven at one block: their code, a bonding curve's
  destination, the pools behind a swap or a liquidity change, and your nonce and balances before
  any spend. A swap quote that falls short of what the proven pools give by more than your slippage
  refuses the review, and so does an exact-output or liquidity review whose numbers disagree with
  them.
- **How far that goes depends on having your own node.** With a monitoring node set, the network's
  RPC must hold the same block, and the review says "confirmed by two nodes". If the two disagree,
  the review is refused; if your node is more than 8 blocks behind, its answers are not used.
  **Without your own node there is no second opinion:** a proof then shows only that the answers
  are consistent with the one node that gave them, and the review claims nothing more.
- **Fees and token imports are cross-checked** when your own node serves reads. A gas price over
  125% of the network RPC's opens the review with both numbers, and a token is imported only if the
  RPC reports the same decimals. A contract you call by its own ABI must have the code the chain's
  state holds.
- **One block on screen.** The header names the newest block, and prices, balances, the trade tape
  and a pair's trades are read at that block ("at #N" in Markets). Screens move when a block
  arrives; through a node on your own network, a block's trades are on screen about a tenth of a
  second after it.
- **Markets.** The Exchange chart is live for every pair, launch-AMM, QuaiSwap and HartiiLabs
  included. A market feed that stops answering recovers by itself. A pool's TVL no longer flips
  between two USD prices, and says how old its QUAI price is. A pool 20 times shallower than
  another for the same two tokens, and under $250, is listed only in Pools. `S` sells to a curve
  from its row.
- **Faster.** Node responses are compressed, and the block a review is proven at is kept ready, so
  a review through your own node does not wait on the network's RPC.

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
