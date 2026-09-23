Quai Terminal is a keyboard-first CLI and TUI wallet for Quai Network.

**This is an alpha.** It is published to be tried, not to be relied on. Read "Before you put funds
in it" below before doing anything with real money.

## What's new in 0.1.0-alpha.3

- **Install with one line.** On Linux or macOS,
  `curl -fsSL https://raw.githubusercontent.com/mpoletiek/quai-terminal/main/install.sh | sh`
  downloads the right build for your computer, checks it against `SHA256SUMS`, and puts
  `quai-terminal` in `~/.local/bin`. `uninstall.sh` removes it again and never deletes your wallets.
- **Node RPC can go through Tor.** With `config set proxy socks5h://127.0.0.1:9050`, requests to a
  public node now go through the proxy like every other lookup, and fail rather than go direct if it
  is down. A node on your own machine or LAN is still reached directly.
- **Built on quai-sdk 0.1.0-alpha.11**, and `diagnostics info` now reports the SDK version the
  build actually uses.

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
  disposable local chain.
- It holds real keys. Back up your recovery phrase before funding anything, and try it with a small
  amount first.
- `wallet watch` creates a watch-only wallet that cannot sign. That is the safe way to look around.

## Start

```sh
./quai-terminal            # the TUI
./quai-terminal --help     # the commands
```
