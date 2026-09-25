#!/bin/sh
# ThanosTokenKiller installer for Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/Bauvater/ThanosTokenKiller/main/install.sh | sh
#
# What it does, in order:
#
#   1. works out which release archive fits this machine,
#   2. downloads it and the checksum file, and refuses to continue if they
#      disagree,
#   3. unpacks `ttk` into ~/.local/bin,
#   4. hands over to `ttk setup`, which does the rest and explains itself.
#
# Nothing is installed system-wide, nothing needs sudo, and nothing is written
# outside ~/.local/bin until `ttk setup` asks you.
#
# Environment:
#   TTK_VERSION=v0.1.0   install a specific release instead of the latest
#   TTK_INSTALL_DIR=…    somewhere other than ~/.local/bin
#   TTK_NO_SETUP=1       just install the binary, skip the walkthrough
#
# POSIX sh on purpose: this is the one file that has to run before anything is
# known about the machine, so it cannot assume bash.

set -eu

REPO="Bauvater/ThanosTokenKiller"
INSTALL_DIR="${TTK_INSTALL_DIR:-$HOME/.local/bin}"

# ---------------------------------------------------------------------------
# Output. Colour only when stdout is a terminal and NO_COLOR is unset.
# ---------------------------------------------------------------------------
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    B=$(printf '\033[1m'); A=$(printf '\033[1;95m'); D=$(printf '\033[90m')
    G=$(printf '\033[32m'); Y=$(printf '\033[33m'); R=$(printf '\033[31m')
    N=$(printf '\033[0m')
else
    B=''; A=''; D=''; G=''; Y=''; R=''; N=''
fi

say()  { printf '%s\n' "$*"; }
step() { printf '%s==>%s %s%s%s\n' "$A" "$N" "$B" "$*" "$N"; }
info() { printf '    %s%s%s\n' "$D" "$*" "$N"; }
ok()   { printf '    %s✓%s %s\n' "$G" "$N" "$*"; }
warn() { printf '    %s!%s %s\n' "$Y" "$N" "$*" >&2; }
die()  { printf '%s✗%s %s\n' "$R" "$N" "$*" >&2; exit 1; }

need() {
    command -v "$1" >/dev/null 2>&1 || die "this installer needs \`$1\` and cannot find it"
}

# ---------------------------------------------------------------------------
# Which build?
# ---------------------------------------------------------------------------
detect_target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Linux) ;;
        Darwin)
            die "there is no macOS build yet. The project only ships binaries it
   tests on CI, and macOS is not covered. Build from source instead:
     git clone https://github.com/$REPO && cd ThanosTokenKiller
     cargo build --release && ./target/release/ttk setup"
            ;;
        *) die "unsupported operating system: $os" ;;
    esac
    case "$arch" in
        x86_64 | amd64) ;;
        *)
            die "there is no $arch build yet — only x86_64. Build from source:
     git clone https://github.com/$REPO && cd ThanosTokenKiller
     cargo build --release && ./target/release/ttk setup"
            ;;
    esac
    # musl is statically linked, so it runs on glibc systems and on Alpine
    # alike. Preferring it removes a whole class of "GLIBC_2.34 not found".
    printf 'x86_64-unknown-linux-musl'
}

latest_version() {
    # The redirect from /releases/latest carries the tag, so this needs no JSON
    # parser and no API token.
    url=$(fetch_effective_url "https://github.com/$REPO/releases/latest")
    printf '%s' "${url##*/}"
}

fetch_effective_url() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSLI -o /dev/null -w '%{url_effective}' "$1"
    else
        # wget prints the redirect chain on stderr; take the last Location.
        wget --spider --max-redirect=10 "$1" 2>&1 |
            sed -n 's/^Location: \([^ ]*\).*/\1/p' | tail -n 1
    fi
}

download() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2"
    else
        wget -qO "$2" "$1"
    fi
}

# ---------------------------------------------------------------------------
say ""
printf '%s  ThanosTokenKiller%s %s— the only Claude Code token killer you will ever need%s\n' \
    "$A" "$N" "$D" "$N"
say ""

command -v curl >/dev/null 2>&1 || command -v wget >/dev/null 2>&1 ||
    die "this installer needs either \`curl\` or \`wget\`"
need tar

TARGET=$(detect_target)
VERSION="${TTK_VERSION:-$(latest_version)}"
case "$VERSION" in
    v*) ;;
    *) die "could not work out the latest version. Set TTK_VERSION=v0.1.0 to pin one." ;;
esac
BARE="${VERSION#v}"
ARCHIVE="ttk-${BARE}-${TARGET}.tar.gz"
BASE="https://github.com/$REPO/releases/download/$VERSION"

step "downloading ttk $VERSION"
info "$TARGET, statically linked"

TMP=$(mktemp -d)
# Leave nothing behind, whether this succeeds or not.
trap 'rm -rf "$TMP"' EXIT INT TERM

download "$BASE/$ARCHIVE" "$TMP/$ARCHIVE" ||
    die "cannot download $BASE/$ARCHIVE
   If that release exists, check your network; otherwise pin one with TTK_VERSION."

# ---------------------------------------------------------------------------
step "verifying the download"
if download "$BASE/SHA256SUMS" "$TMP/SHA256SUMS" 2>/dev/null; then
    if command -v sha256sum >/dev/null 2>&1; then
        expected=$(grep " $ARCHIVE\$" "$TMP/SHA256SUMS" | awk '{print $1}' || true)
        actual=$(sha256sum "$TMP/$ARCHIVE" | awk '{print $1}')
    elif command -v shasum >/dev/null 2>&1; then
        expected=$(grep " $ARCHIVE\$" "$TMP/SHA256SUMS" | awk '{print $1}' || true)
        actual=$(shasum -a 256 "$TMP/$ARCHIVE" | awk '{print $1}')
    else
        expected=''
        warn "no sha256sum or shasum on this machine — cannot verify the download"
    fi

    if [ -n "${expected:-}" ]; then
        [ "$expected" = "$actual" ] ||
            die "checksum mismatch. Do not run this binary.
   expected $expected
   got      $actual"
        ok "sha256 matches the published checksum"
    elif [ -n "${actual:-}" ]; then
        warn "$ARCHIVE is not listed in SHA256SUMS; continuing unverified"
    fi
else
    warn "no SHA256SUMS published for $VERSION; continuing unverified"
fi

# ---------------------------------------------------------------------------
step "installing"
tar -xzf "$TMP/$ARCHIVE" -C "$TMP"
BIN=$(find "$TMP" -type f -name ttk -perm -u+x | head -n 1)
[ -n "$BIN" ] || die "the archive did not contain a ttk binary"

mkdir -p "$INSTALL_DIR"
# Install to a temporary name and rename, so a half written binary is never
# left where the old working one used to be.
cp "$BIN" "$INSTALL_DIR/.ttk.new"
chmod +x "$INSTALL_DIR/.ttk.new"
mv "$INSTALL_DIR/.ttk.new" "$INSTALL_DIR/ttk"
ok "$INSTALL_DIR/ttk"

installed=$("$INSTALL_DIR/ttk" --version 2>/dev/null) ||
    die "the installed binary does not run on this machine"
ok "$installed"

# ---------------------------------------------------------------------------
if [ -n "${TTK_NO_SETUP:-}" ]; then
    say ""
    info "run \`$INSTALL_DIR/ttk setup\` when you are ready."
    exit 0
fi

say ""
step "setting up"
info "ttk takes it from here — every step asks first."
say ""
exec "$INSTALL_DIR/ttk" setup
