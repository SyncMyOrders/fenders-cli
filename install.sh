#!/bin/sh
# Install, update or uninstall the fenders CLI on macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/SyncMyOrders/fenders-cli/main/install.sh | sh
#
# Options (or environment variables):
#   --version vX.Y.Z   FENDERS_VERSION      install a specific release (default: latest)
#   --dir PATH         FENDERS_INSTALL_DIR  install directory (default: where fenders
#                                           already is, else ~/.local/bin)
#   --uninstall                             remove the binary
#   --purge                                 with --uninstall, also remove ~/.config/fenders
#                      FENDERS_RELEASES_URL  release mirror (default: GitHub releases)
#
# Re-running the script updates an existing installation in place.

set -eu

REPO="SyncMyOrders/fenders-cli"
BIN="fenders"
RELEASES="${FENDERS_RELEASES_URL:-https://github.com/$REPO/releases}"
# Refuse plain HTTP unless a mirror was explicitly configured.
if [ -n "${FENDERS_RELEASES_URL:-}" ]; then CURL_PROTO=""; else CURL_PROTO="--proto =https"; fi

version="${FENDERS_VERSION:-}"
dir="${FENDERS_INSTALL_DIR:-}"
uninstall=0
purge=0

if [ -t 2 ] && [ -z "${NO_COLOR:-}" ]; then
  ORANGE="$(printf '\033[38;2;255;96;20m')"; GREEN="$(printf '\033[38;2;111;211;165m')"
  RED="$(printf '\033[38;2;240;82;64m')"; DIM="$(printf '\033[38;2;169;182;192m')"; RESET="$(printf '\033[0m')"
else
  ORANGE=""; GREEN=""; RED=""; DIM=""; RESET=""
fi
info() { printf '%s›%s %s\n' "$ORANGE" "$RESET" "$*" >&2; }
ok() { printf '%s✓%s %s\n' "$GREEN" "$RESET" "$*" >&2; }
hint() { printf '  %s%s%s\n' "$DIM" "$*" "$RESET" >&2; }
die() { printf '%s✗%s %s\n' "$RED" "$RESET" "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --version) [ $# -ge 2 ] || die "--version needs a value"; version="$2"; shift 2 ;;
    --version=*) version="${1#*=}"; shift ;;
    --dir) [ $# -ge 2 ] || die "--dir needs a value"; dir="$2"; shift 2 ;;
    --dir=*) dir="${1#*=}"; shift ;;
    --uninstall) uninstall=1; shift ;;
    --purge) purge=1; shift ;;
    -h|--help) sed -n '2,15p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown option: $1 (see --help)" ;;
  esac
done

have() { command -v "$1" >/dev/null 2>&1; }

# Default to updating wherever fenders already lives.
if [ -z "$dir" ]; then
  if existing="$(command -v "$BIN" 2>/dev/null)" && [ -n "$existing" ]; then
    dir="$(dirname "$existing")"
  else
    dir="$HOME/.local/bin"
  fi
fi
target_path="$dir/$BIN"

# Use sudo only when the directory isn't writable by us.
SUDO=""
needs_sudo() {
  probe="$dir"
  while [ ! -d "$probe" ]; do probe="$(dirname "$probe")"; done
  [ ! -w "$probe" ]
}
if needs_sudo; then
  have sudo || die "$dir is not writable; re-run with --dir or as a user who can write there"
  SUDO="sudo"
  info "Installing to $dir requires sudo"
fi

if [ "$uninstall" -eq 1 ]; then
  if [ -e "$target_path" ]; then
    $SUDO rm -f "$target_path"
    ok "Removed $target_path"
  else
    info "No $BIN found at $target_path"
  fi
  config="${FENDERS_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/fenders}"
  if [ "$purge" -eq 1 ]; then
    rm -rf "$config"
    ok "Removed $config"
    hint "On macOS the API key stays in Keychain (\"fenders-cli\"); run 'fenders auth logout' before uninstalling to remove it."
  elif [ -d "$config" ]; then
    hint "Configuration kept in $config (use --purge to remove it)."
  fi
  exit 0
fi

# Pick the release asset for this machine.
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Darwin)
    # Prefer native arm64 even when this shell runs under Rosetta.
    if [ "$arch" = "x86_64" ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" = "1" ]; then
      arch="arm64"
    fi
    case "$arch" in
      arm64|aarch64) target="aarch64-apple-darwin" ;;
      x86_64) target="x86_64-apple-darwin" ;;
      *) die "unsupported macOS architecture: $arch" ;;
    esac ;;
  Linux)
    case "$arch" in
      x86_64|amd64) target="x86_64-unknown-linux-musl" ;;
      aarch64|arm64) target="aarch64-unknown-linux-musl" ;;
      *) die "unsupported Linux architecture: $arch (build from source with 'cargo install --git https://github.com/$REPO')" ;;
    esac ;;
  *) die "unsupported OS: $os — on Windows, download the .zip from https://github.com/$REPO/releases" ;;
esac

if have curl; then
  # shellcheck disable=SC2086 # CURL_PROTO is intentionally split
  fetch() { curl -fsSL $CURL_PROTO --retry 3 -o "$2" "$1"; }
  latest_tag() {
    # shellcheck disable=SC2086
    url="$(curl -fsSLI $CURL_PROTO -o /dev/null -w '%{url_effective}' "$RELEASES/latest")" || return 1
    case "$url" in */tag/*) printf '%s\n' "${url##*/tag/}" ;; *) return 1 ;; esac
  }
elif have wget; then
  fetch() { wget -q -O "$2" "$1"; }
  latest_tag() {
    wget -q --https-only -O - "https://api.github.com/repos/$REPO/releases/latest" |
      sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1
  }
else
  die "curl or wget is required"
fi

if [ -z "$version" ]; then
  version="$(latest_tag)" || true
  [ -n "$version" ] || die "could not find the latest release of $REPO (none published yet?)"
fi
case "$version" in v*) ;; *) version="v$version" ;; esac

current=""
if [ -x "$target_path" ]; then
  current="$("$target_path" --version 2>/dev/null | awk '{print $2}')" || current=""
  if [ "v$current" = "$version" ]; then
    ok "$BIN $version is already installed at $target_path"
    exit 0
  fi
fi

asset="$BIN-$version-$target.tar.gz"
base="$RELEASES/download/$version"
tmp="$(mktemp -d 2>/dev/null || mktemp -d -t fenders)"
trap 'rm -rf "$tmp"' EXIT INT TERM

info "Downloading $BIN $version for $target"
fetch "$base/$asset" "$tmp/$asset" || die "download failed: $base/$asset"

# Verify the checksum when the release publishes one.
if fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" 2>/dev/null; then
  expected="$(awk -v f="$asset" '$2 == f || $2 == "*"f {print $1}' "$tmp/SHA256SUMS")"
  [ -n "$expected" ] || die "$asset is not listed in SHA256SUMS"
  if have sha256sum; then
    actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
  elif have shasum; then
    actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
  else
    die "sha256sum or shasum is required to verify the download"
  fi
  [ "$expected" = "$actual" ] || die "checksum mismatch for $asset"
  ok "Checksum verified"
else
  info "No SHA256SUMS published for $version; skipping verification"
fi

tar -xzf "$tmp/$asset" -C "$tmp"
src="$tmp/$BIN-$version-$target/$BIN"
[ -f "$src" ] || die "archive did not contain $BIN"
chmod 755 "$src"

$SUDO mkdir -p "$dir"
# Write next to the target, then rename: atomic, and safe while fenders is running.
$SUDO cp "$src" "$target_path.new"
$SUDO mv -f "$target_path.new" "$target_path"

if [ -n "$current" ]; then
  ok "Updated $BIN v$current → $version ($target_path)"
else
  ok "Installed $BIN $version to $target_path"
fi

case ":$PATH:" in
  *":$dir:"*) ;;
  *)
    hint "$dir is not on your PATH. Add it with:"
    hint "  echo 'export PATH=\"$dir:\$PATH\"' >> ~/.profile   # or ~/.zshrc"
    ;;
esac
[ -n "$current" ] || hint "Get started: $BIN auth login"
