#!/bin/sh
# Uninstall Quai Terminal.
#
#   curl -fsSL https://raw.githubusercontent.com/mpoletiek/quai-terminal/main/uninstall.sh | sh
#
# It stops the background daemon, removes the systemd user unit if you installed one, and removes
# the `quai-terminal` binary. It never deletes your wallets: their data directory holds your keys,
# so it is only shown, with how to delete it yourself once you are sure you have your recovery
# phrases.
#
# Settings, as environment variables (put them before `sh`):
#   QUAI_TERMINAL_INSTALL_DIR  where install.sh put the binary (default: ~/.local/bin)

set -eu

BIN="quai-terminal"
UNIT="$HOME/.config/systemd/user/quai-terminal.service"

say() { printf '%s\n' "$*"; }
fail() { printf 'uninstall: %s\n' "$*" >&2; exit 1; }

# Read in full before anything runs, so a download cut off halfway cannot run half of it.
main() {
    dir="${QUAI_TERMINAL_INSTALL_DIR:-$HOME/.local/bin}"
    binary="$dir/$BIN"
    if [ ! -f "$binary" ]; then
        found="$(command -v "$BIN" 2>/dev/null || true)"
        [ -n "$found" ] || fail "no $BIN in $dir or on your PATH; nothing to uninstall"
        binary="$found"
    fi
    # Only ever remove a file that says it is Quai Terminal.
    version="$("$binary" --version 2>/dev/null || true)"
    case "$version" in
        "$BIN "*) ;;
        *) fail "$binary does not identify itself as Quai Terminal; leaving it alone" ;;
    esac

    # Where the wallet keeps its data, worked out here rather than asked of the binary: running it
    # creates that directory, and someone who never used it would then be told of wallets in a
    # folder the uninstaller had just made.
    data_dirs
    say "Removing $version from $binary"
    # A daemon needs a data directory, so with none there is nothing to stop. One left running
    # would keep watching wallets with no binary behind it.
    if [ -n "$existing" ]; then
        stopped="$("$binary" daemon stop 2>/dev/null || true)"
        case "$stopped" in
            "" | *"not running"*) ;;
            *) say "Stopped the background daemon." ;;
        esac
        # Running it may have carried a pre-rename `quai-wallet` directory over to the new name:
        # report where the data is now, not where it was.
        data_dirs
    fi

    if [ -f "$UNIT" ] && grep -q "$BIN" "$UNIT" 2>/dev/null; then
        if command -v systemctl >/dev/null 2>&1; then
            systemctl --user disable --now quai-terminal.service >/dev/null 2>&1 || true
            rm -f "$UNIT"
            systemctl --user daemon-reload >/dev/null 2>&1 || true
        else
            rm -f "$UNIT"
        fi
        say "Removed the systemd user unit $UNIT."
    fi

    rm -f "$binary"
    say "Removed $binary."
    say ""
    if [ -n "$existing" ]; then
        say "Your wallet data was not deleted. It holds your encrypted keys:"
        printf '%s\n' "$existing" | while IFS= read -r d; do say "    $d"; done
        say ""
        say "Delete it only once you have every wallet's recovery phrase written down, or the funds in"
        say "those wallets are lost for good. To delete it:"
        say ""
        printf '%s\n' "$existing" | while IFS= read -r d; do say "    rm -rf \"$d\""; done
    else
        say "No wallet data was found, so there is nothing else to remove."
    fi
}

# Sets `existing` to the data directories that exist, one per line: QUAI_TERMINAL_HOME (or the
# older QUAI_WALLET_HOME) when set, else the platform's, plus a pre-rename `quai-wallet` one the
# wallet has not adopted yet. The same places the wallet itself looks (wallet-core `paths.rs`).
data_dirs() {
    existing=""
    home_override="${QUAI_TERMINAL_HOME:-${QUAI_WALLET_HOME:-}}"
    if [ -n "$home_override" ]; then
        candidates="$home_override"
    else
        case "$(uname -s)" in
            Darwin) candidates="$HOME/Library/Application Support/network.quai.quai-terminal
$HOME/Library/Application Support/network.quai.quai-wallet" ;;
            *) candidates="${XDG_DATA_HOME:-$HOME/.local/share}/quai-terminal
${XDG_DATA_HOME:-$HOME/.local/share}/quai-wallet" ;;
        esac
    fi
    existing="$(printf '%s\n' "$candidates" | while IFS= read -r d; do if [ -d "$d" ]; then printf '%s\n' "$d"; fi; done)"
}

main "$@"
