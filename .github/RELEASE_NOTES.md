Quai Terminal is a keyboard-first CLI and TUI wallet for Quai Network.

**This is an alpha.** It is published to be tried, not to be relied on. Read "Before you put funds
in it" below before doing anything with real money.

## What's new in 0.1.0-alpha.5

- **Limit orders that explain themselves and are watched.** Quai has no order book, so an order is
  the wallet waiting for a price on your behalf. Nothing is posted on-chain and nothing is signed
  without you. Set a target as `+5%` or an amount to receive. The dialog shows the price now and
  at the target, and what the order guarantees after slippage. Active orders are re-checked every
  30 seconds by the open terminal or, when it is closed, the background daemon. You are told once
  when one is reachable, and Trade › Orders prepares the review. From the shell:
  `quai-terminal order create quai usdt 1 --target +5%`.
- **NFTs the explorer lost are filled in.** When explorer.qu.ai failed to read an NFT's metadata,
  it showed as just "#241". It is now read from where the NFT's contract says it is, through your
  IPFS gateway (`ipfs://` only, never a host the creator chose).
- **Collections show every item.** Explore loads a collection's items as you scroll, instead of
  stopping at the first 48.
- **Esc leaves a conversion.** On Convert or Wrap, Esc goes back to the swap card and its pair.
- **The lock screen loops its animations again.** Settings › "Loop the lock screen animation"
  turns it off to play one per lock.

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
