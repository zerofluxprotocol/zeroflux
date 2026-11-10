#!/bin/sh
# ZFX VPN installer: builds from source on this machine.
#   curl -fsSL https://raw.githubusercontent.com/zerofluxprotocol/zeroflux/main/vpn/install.sh | sh
# Options (environment; the PROMPTVPN_* names from Prompt VPN 0.1 still work):
#   ZFX_VPN_REF=main               branch or tag to build
#   ZFX_VPN_PREFIX=~/.local/bin    where to put the binary
#   ZFX_VPN_SRC=~/.zfx-vpn/src     where to keep the source checkout
#   ZFX_VPN_INSTALL_RUST=1         install Rust via rustup without asking (non-interactive)
# Needs git and a C toolchain (Xcode Command Line Tools on macOS). Installs nothing
# system-wide and never uses sudo. Re-run to update.
#
# Upgrading from Prompt VPN: the binary is installed as `zfx-vpn` and
# `promptvpn` becomes a link to it (it prints a deprecation note). The first
# `zfx-vpn` command migrates the Prompt VPN data in place; `zfx-vpn migrate
# rollback` undoes that. Stop a running Prompt VPN daemon first.
set -eu

REPO="https://github.com/zerofluxprotocol/zeroflux.git"
REF="${ZFX_VPN_REF:-${PROMPTVPN_REF:-main}}"
PREFIX="${ZFX_VPN_PREFIX:-${PROMPTVPN_PREFIX:-$HOME/.local/bin}}"
SRC="${ZFX_VPN_SRC:-${PROMPTVPN_SRC:-$HOME/.zfx-vpn/src}}"

say() { printf '\033[1m%s\033[0m\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
ask() { # ask "question" -> 0 if yes; reads the terminal even when piped from curl
  [ -r /dev/tty ] || return 1
  printf '%s [y/N] ' "$1" > /dev/tty
  read -r a < /dev/tty || return 1
  case "$a" in y|Y|yes) return 0 ;; *) return 1 ;; esac
}

case "$(uname -s)" in
  Darwin) os=macos ;;
  Linux) os=linux ;;
  *) die "unsupported OS $(uname -s); ZFX VPN supports macOS and Linux" ;;
esac

command -v git >/dev/null 2>&1 || die "git is required${os:+ (macOS: xcode-select --install)}"
if ! command -v cc >/dev/null 2>&1; then
  [ "$os" = macos ] && die "a C compiler is required: run 'xcode-select --install' and re-run this script"
  die "a C compiler is required (e.g. apt install build-essential)"
fi

[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
if ! command -v cargo >/dev/null 2>&1; then
  say "Rust is needed to build ZFX VPN and is not installed."
  echo "It will be installed for your user only, via the official installer from https://rustup.rs"
  if [ "${ZFX_VPN_INSTALL_RUST:-${PROMPTVPN_INSTALL_RUST:-}}" = 1 ] || ask "Install Rust now?"; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path
    . "$HOME/.cargo/env"
  else
    die "install Rust from https://rustup.rs and re-run"
  fi
fi

say "Fetching ZFX VPN ($REF)"
if [ -d "$SRC/.git" ]; then
  git -C "$SRC" fetch --depth 1 origin "$REF" && git -C "$SRC" checkout -q FETCH_HEAD
else
  mkdir -p "$(dirname "$SRC")"
  git clone --depth 1 --branch "$REF" "$REPO" "$SRC"
fi

say "Building (first build takes a few minutes)"
cd "$SRC/vpn"
cargo build --release --locked -p pv-cli

mkdir -p "$PREFIX"
install -m 0755 target/release/zfx-vpn "$PREFIX/zfx-vpn"
# Deprecated name for existing scripts, services and muscle memory.
if [ -e "$PREFIX/promptvpn" ] && [ ! -L "$PREFIX/promptvpn" ]; then
  mv "$PREFIX/promptvpn" "$PREFIX/promptvpn.0.1.bak"
  echo "Kept the old Prompt VPN binary as $PREFIX/promptvpn.0.1.bak (for rollback)."
fi
ln -sf "$PREFIX/zfx-vpn" "$PREFIX/promptvpn"
say "Installed $PREFIX/zfx-vpn ($("$PREFIX/zfx-vpn" --version)); promptvpn -> zfx-vpn"
case ":$PATH:" in
  *":$PREFIX:"*) ;;
  *) echo "Add it to your PATH:  export PATH=\"$PREFIX:\$PATH\"" ;;
esac
echo
echo "Next:"
echo "  zfx-vpn init            # guided setup (or: zfx-vpn init --dashboard)"
echo "  zfx-vpn connect test    # prove protection with synthetic data"
echo "  zfx-vpn dashboard"
