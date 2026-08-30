#!/bin/sh
# Install bedouin.
#
#   curl -fsSL https://samishal1998.github.io/bedouin/install.sh | sh
#
# Set BEDOUIN_VERSION to pin a release, BEDOUIN_BIN_DIR to choose where it
# lands. Everything this script does is printed before it does it.
set -eu

REPO="samishal1998/bedouin"
VERSION="${BEDOUIN_VERSION:-latest}"
BIN_DIR="${BEDOUIN_BIN_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
die() { printf 'install: %s\n' "$*" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || die "this needs $1, which is not installed"; }
need uname
need tar
if command -v curl >/dev/null 2>&1; then fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then fetch() { wget -qO "$2" "$1"; }
else die "this needs curl or wget"; fi

# Pick the build. musl on Linux, so it runs on a machine with nothing
# installed -- which is the machine bedouin is for.
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Linux)  case "$arch" in
            x86_64|amd64)  target=x86_64-unknown-linux-musl ;;
            aarch64|arm64) target=aarch64-unknown-linux-musl ;;
            *) die "no build for $os/$arch yet. Build from source: cargo install --git https://github.com/$REPO" ;;
          esac ;;
  Darwin) case "$arch" in
            x86_64)        target=x86_64-apple-darwin ;;
            arm64)         target=aarch64-apple-darwin ;;
            *) die "no build for $os/$arch yet" ;;
          esac ;;
  *) die "$os is not supported. bedouin targets Linux and macOS" ;;
esac

if [ "$VERSION" = "latest" ]; then
  url="https://github.com/$REPO/releases/latest/download/bedouin-$target.tar.gz"
else
  url="https://github.com/$REPO/releases/download/$VERSION/bedouin-$target.tar.gz"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "  target   $target"
say "  from     $url"
say "  into     $BIN_DIR/bedouin"
say ""

fetch "$url" "$tmp/bedouin.tar.gz" || die "could not download $url
  If this is a fresh repository, there may be no release yet.
  Build from source instead: cargo install --git https://github.com/$REPO"

# Verify against the release's own checksums when they are published.
sums_url="${url%/bedouin-$target.tar.gz}/SHA256SUMS"
if fetch "$sums_url" "$tmp/SHA256SUMS" 2>/dev/null; then
  if command -v sha256sum >/dev/null 2>&1; then sum="$(sha256sum "$tmp/bedouin.tar.gz" | cut -d' ' -f1)"
  elif command -v shasum >/dev/null 2>&1; then sum="$(shasum -a 256 "$tmp/bedouin.tar.gz" | cut -d' ' -f1)"
  else sum=""; fi
  if [ -n "$sum" ]; then
    grep -q "$sum" "$tmp/SHA256SUMS" || die "checksum mismatch -- refusing to install this"
    say "  checksum ok"
  fi
fi

tar -xzf "$tmp/bedouin.tar.gz" -C "$tmp"
mkdir -p "$BIN_DIR"
mv "$tmp/bedouin" "$BIN_DIR/bedouin"
chmod +x "$BIN_DIR/bedouin"

say "Installed $("$BIN_DIR/bedouin" --version)"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) say ""
     say "$BIN_DIR is not on your PATH. Add it:"
     say "  export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac

say ""
say "Next:"
say "  bedouin init     write a starter config"
say "  bedouin plan     see what it would do -- it changes nothing"
