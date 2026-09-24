Quai Terminal is a keyboard-first CLI and TUI wallet for Quai Network.

**This is an alpha.** It is published to be tried, not to be relied on. Read "Before you put funds
in it" below before doing anything with real money.

## What's new in 0.1.0-alpha.8

- **A token's bonding curve is quoted beside the exchanges.** When you swap QUAI for a token that
  trades on a Quainance or HartiiLabs curve, the swap card quotes the curve too, says how deep it
  is, and trades on the curve when it pays more or when the exchanges cannot fill the amount. QAXE
  is the case in point: its market is its curve (about $3.9k), and a separate $13 pool beside it was
  all the swap card could see, so 1,000 QUAI was refused there although the curve fills it.
- **More of Quainance's trade zone.** Tokens from Quainance's second launcher (its revenue system)
  can be bought and sold on their curves, and every pair on Quainance's exchanges is listed,
  including new and small ones the explorer leaves out. QuaiSwap lists the trade zone's pools
  (BARRY, BOSS, Q0, QPEPE) and QIQI, and nothing else. poop.fun curves are not supported.
- **The revenue AMM is named for what it is.** The exchange shown as "Hartii AMM" is Quainance's
  revenue AMM; HartiiLabs' tokens trade on their own curves.
- **Markets numbers you can trust.** Sorting by 24h change or TVL orders by the figures the rows
  show, from the top of the list. A pair's 24h change is a real day's change: history that does not
  reach a day back no longer produces one. 24h volume, trades, high and low no longer change with
  the chart's timeframe. Launch progress means the same thing on every row.
- **Cursors stay put.** Pools, Launches and the trade tape keep the cursor on the pool, token or
  trade it was on when their lists refresh, so an action never lands on a row that moved under it.
- **Smaller fixes.** The token picker ranks what you typed first (`qi` finds Qi), Convert stops
  claiming a route pays more than one that cannot run, curve quotes work from a watch-only wallet,
  and `markets PUNK` finds the pair from its symbol.

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
- It holds real keys. Back up your recovery phrase before funding anything, and try it with a small
  amount first.
- `wallet watch` creates a watch-only wallet that cannot sign. That is the safe way to look around.

## Start

```sh
./quai-terminal            # the TUI
./quai-terminal --help     # the commands
```
