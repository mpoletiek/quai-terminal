#!/bin/sh
# Install Quai Terminal from its GitHub releases.
#
#   curl -fsSL https://raw.githubusercontent.com/mpoletiek/quai-terminal/main/install.sh | sh
#
# It downloads the build for this computer, checks it against the release's SHA256SUMS, and
# puts `quai-terminal` in ~/.local/bin. Nothing runs as root, and nothing outside that directory
# is changed: if it is not on your PATH, the line to add is printed, not written for you.
#
# Settings, as environment variables (put them before `sh`, e.g. `| QUAI_TERMINAL_VERSION=v0.1.0-alpha.2 sh`):
#   QUAI_TERMINAL_VERSION      a release tag to install (default: the newest release, alphas included)
#   QUAI_TERMINAL_INSTALL_DIR  where the binary goes (default: ~/.local/bin)
#
# Supported: Linux x86-64, macOS on Apple Silicon and Intel.

set -eu

REPO="mpoletiek/quai-terminal"
BIN="quai-terminal"

say() { printf '%s\n' "$*"; }
fail() { printf 'install: %s\n' "$*" >&2; exit 1; }

# The whole script is read before any of it runs, so a download cut off halfway cannot run half an
# installer.
main() {
    need uname
    need tar
    need mktemp

    fetch_tool
    platform
    version="${QUAI_TERMINAL_VERSION:-}"
    if [ -z "$version" ]; then
        version="$(newest_release)"
    fi
    case "$version" in
        v*) ;;
        *) version="v$version" ;;
    esac
    dir="${QUAI_TERMINAL_INSTALL_DIR:-$HOME/.local/bin}"

    stage="$BIN-${version#v}-$target"
    base="https://github.com/$REPO/releases/download/$version"
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' EXIT INT TERM

    say "Installing Quai Terminal $version ($target) into $dir"
    download "$base/$stage.tar.gz" "$work/$stage.tar.gz" || fail "no $target build in release $version ($base/$stage.tar.gz)"
    download "$base/SHA256SUMS" "$work/SHA256SUMS" || fail "release $version has no SHA256SUMS; not installing an unchecked download"
    verify "$work" "$stage.tar.gz"

    tar -xzf "$work/$stage.tar.gz" -C "$work"
    [ -f "$work/$stage/$BIN" ] || fail "the archive does not contain $stage/$BIN"

    mkdir -p "$dir"
    # Replace the binary in one rename, so a running copy is never overwritten in place.
    cp "$work/$stage/$BIN" "$dir/.$BIN.new"
    chmod 755 "$dir/.$BIN.new"
    mv -f "$dir/.$BIN.new" "$dir/$BIN"
    if [ "$os" = "Darwin" ] && command -v xattr >/dev/null 2>&1; then
        # A browser download is quarantined by Gatekeeper; this one was checked above.
        xattr -d com.apple.quarantine "$dir/$BIN" 2>/dev/null || true
    fi

    installed="$("$dir/$BIN" --version 2>/dev/null)" || fail "$dir/$BIN was installed but does not run on this system"
    say "Installed $installed"
    say ""
    on_path "$dir"
    say "Start it with:   $BIN"
    say "Remove it with:  curl -fsSL https://raw.githubusercontent.com/$REPO/main/uninstall.sh | sh"
    say ""
    say "This is an alpha. Back up your recovery phrase before funding a wallet, and try it with a"
    say "small amount first: https://github.com/$REPO/releases/tag/$version"
}

need() {
    command -v "$1" >/dev/null 2>&1 || fail "this needs '$1', which was not found"
}

fetch_tool() {
    if command -v curl >/dev/null 2>&1; then
        fetcher=curl
    elif command -v wget >/dev/null 2>&1; then
        fetcher=wget
    else
        fail "this needs curl or wget"
    fi
}

# download URL FILE
download() {
    if [ "$fetcher" = curl ]; then
        curl -fsSL --proto '=https' --tlsv1.2 --retry 3 -o "$2" "$1"
    else
        wget -q --https-only -O "$2" "$1"
    fi
}

platform() {
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
        Linux)
            case "$arch" in
                x86_64 | amd64) target="linux-x86_64" ;;
                *) fail "there is no Linux $arch build yet (only x86-64); build from source instead" ;;
            esac
            ;;
        Darwin)
            # A shell running under Rosetta reports x86_64 on Apple Silicon; the native build is
            # the one to install there.
            if [ "$arch" = "arm64" ] || [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = "1" ]; then
                target="macos-apple-silicon"
            else
                target="macos-intel"
            fi
            ;;
        *) fail "there is no build for $os; Quai Terminal runs on Linux and macOS" ;;
    esac
}

# The newest release, prereleases included. GitHub's "latest release" skips prereleases, and every
# release so far is an alpha, so the list is read instead.
newest_release() {
    api="https://api.github.com/repos/$REPO/releases?per_page=1"
    if [ "$fetcher" = curl ]; then
        body="$(curl -fsSL --proto '=https' --tlsv1.2 -H 'Accept: application/vnd.github+json' "$api")" || body=""
    else
        body="$(wget -q --https-only -O - "$api")" || body=""
    fi
    tag="$(printf '%s' "$body" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
    [ -n "$tag" ] || fail "could not ask GitHub for the newest release (rate limited or offline?); set QUAI_TERMINAL_VERSION to a tag, e.g. v0.1.0-alpha.2"
    printf '%s' "$tag"
}

# verify DIR FILE: FILE must appear in DIR/SHA256SUMS with a matching hash.
verify() {
    expected="$(awk -v f="$2" '$2 == f || $2 == "*" f { print $1 }' "$1/SHA256SUMS")"
    [ -n "$expected" ] || fail "SHA256SUMS does not list $2"
    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$1/$2" | awk '{ print $1 }')"
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "$1/$2" | awk '{ print $1 }')"
    else
        fail "found neither sha256sum nor shasum to check the download with; not installing it unchecked"
    fi
    [ "$expected" = "$actual" ] || fail "checksum mismatch for $2 (expected $expected, got $actual); nothing was installed"
    say "Checksum verified."
}

on_path() {
    case ":$PATH:" in
        *":$1:"*) ;;
        *)
            say "$1 is not on your PATH. Add it by putting this line in your shell's startup file"
            say "(~/.bashrc, ~/.zshrc, or ~/.profile), then open a new terminal:"
            say ""
            say "    export PATH=\"$1:\$PATH\""
            say ""
            ;;
    esac
}

main "$@"
