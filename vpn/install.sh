#!/bin/sh
# Prompt VPN installer: builds from source on this machine.
#   curl -fsSL https://raw.githubusercontent.com/numinousmuses/promptvpn/main/install.sh | sh
# Options (environment):
#   PROMPTVPN_REF=main            branch or tag to build
#   PROMPTVPN_PREFIX=~/.local/bin where to put the binary
#   PROMPTVPN_SRC=~/.promptvpn/src where to keep the source checkout
#   PROMPTVPN_INSTALL_RUST=1      install Rust via rustup without asking (non-interactive)
# Needs git and a C toolchain (Xcode Command Line Tools on macOS). Installs nothing
# system-wide and never uses sudo. Re-run to update.
set -eu

REPO="https://github.com/numinousmuses/promptvpn.git"
REF="${PROMPTVPN_REF:-main}"
PREFIX="${PROMPTVPN_PREFIX:-$HOME/.local/bin}"
SRC="${PROMPTVPN_SRC:-$HOME/.promptvpn/src}"

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
  *) die "unsupported OS $(uname -s); Prompt VPN supports macOS and Linux" ;;
esac

command -v git >/dev/null 2>&1 || die "git is required${os:+ (macOS: xcode-select --install)}"
if ! command -v cc >/dev/null 2>&1; then
  [ "$os" = macos ] && die "a C compiler is required: run 'xcode-select --install' and re-run this script"
  die "a C compiler is required (e.g. apt install build-essential)"
fi

[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
if ! command -v cargo >/dev/null 2>&1; then
  say "Rust is needed to build Prompt VPN and is not installed."
  echo "It will be installed for your user only, via the official installer from https://rustup.rs"
  if [ "${PROMPTVPN_INSTALL_RUST:-}" = 1 ] || ask "Install Rust now?"; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path
    . "$HOME/.cargo/env"
  else
    die "install Rust from https://rustup.rs and re-run"
  fi
fi

say "Fetching Prompt VPN ($REF)"
if [ -d "$SRC/.git" ]; then
  git -C "$SRC" fetch --depth 1 origin "$REF" && git -C "$SRC" checkout -q FETCH_HEAD
else
  mkdir -p "$(dirname "$SRC")"
  git clone --depth 1 --branch "$REF" "$REPO" "$SRC"
fi

say "Building (first build takes a few minutes)"
cd "$SRC"
cargo build --release --locked -p pv-cli

mkdir -p "$PREFIX"
install -m 0755 target/release/promptvpn "$PREFIX/promptvpn"
say "Installed $PREFIX/promptvpn ($("$PREFIX/promptvpn" --version))"
case ":$PATH:" in
  *":$PREFIX:"*) ;;
  *) echo "Add it to your PATH:  export PATH=\"$PREFIX:\$PATH\"" ;;
esac
echo
echo "Next:"
echo "  promptvpn init            # guided setup (or: promptvpn init --dashboard)"
echo "  promptvpn connect test    # prove protection with synthetic data"
echo "  promptvpn dashboard"
