#!/bin/sh
# JeTTY one-line installer — downloads the latest prebuilt release (no Rust
# toolchain needed) and installs it for the current user.
#
#   curl -fsSL https://raw.githubusercontent.com/bozdemir/jetty/main/install.sh | sh
#
# Installs to ~/.local (binary on PATH, icons, and a .desktop launcher entry).
# Set JETTY_PREFIX=/usr/local and run with sudo for a system-wide install.
set -eu

REPO="bozdemir/jetty"
PREFIX="${JETTY_PREFIX:-$HOME/.local}"

say()  { printf '\033[1;35m::\033[0m %s\n' "$1"; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

# --- platform check ---
os="$(uname -s)"; arch="$(uname -m)"
[ "$os" = "Linux" ]  || die "JeTTY currently ships prebuilt binaries for Linux only (got $os). Build from source: https://github.com/$REPO"
[ "$arch" = "x86_64" ] || die "no prebuilt binary for $arch yet — build from source: https://github.com/$REPO"

command -v curl >/dev/null 2>&1 || die "curl is required"
command -v tar  >/dev/null 2>&1 || die "tar is required"

# --- resolve the latest release tag ---
say "Finding the latest JeTTY release…"
tag="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
        | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)"
[ -n "$tag" ] || die "could not find a release. See https://github.com/$REPO/releases"
ver="${tag#v}"

asset="jetty-${ver}-x86_64-linux.tar.gz"
url="https://github.com/$REPO/releases/download/$tag/$asset"

# --- download + extract ---
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
say "Downloading $asset…"
curl -fSL "$url" -o "$tmp/jetty.tar.gz" || die "download failed: $url"
tar -C "$tmp" -xzf "$tmp/jetty.tar.gz"
src="$tmp/jetty-${ver}-x86_64-linux"
[ -x "$src/jetty" ] || die "archive layout unexpected (missing jetty binary)"

# --- install ---
say "Installing to $PREFIX…"
install -Dm755 "$src/jetty" "$PREFIX/bin/jetty"
for sz in 16 32 48 64 128 256; do
  icon="$src/assets/icons/jetty-${sz}.png"
  [ -f "$icon" ] && install -Dm644 "$icon" "$PREFIX/share/icons/hicolor/${sz}x${sz}/apps/jetty.png"
done
[ -f "$src/assets/jetty.desktop" ] && install -Dm644 "$src/assets/jetty.desktop" "$PREFIX/share/applications/jetty.desktop"

gtk-update-icon-cache "$PREFIX/share/icons/hicolor" >/dev/null 2>&1 || true
update-desktop-database "$PREFIX/share/applications" >/dev/null 2>&1 || true

say "JeTTY $tag installed → $PREFIX/bin/jetty"
case ":$PATH:" in
  *":$PREFIX/bin:"*) ;;
  *) printf '\033[1;33mnote:\033[0m add %s to your PATH:\n  export PATH="%s:$PATH"\n' "$PREFIX/bin" "$PREFIX/bin" ;;
esac
printf 'Launch it with: \033[1;36mjetty\033[0m   (press F9 to summon)\n'
