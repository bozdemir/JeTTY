#!/bin/sh
# JeTTY one-line installer — downloads the latest prebuilt release (no Rust
# toolchain needed) and installs it for the current user.
#
#   curl -fsSL https://raw.githubusercontent.com/bozdemir/JeTTY/main/install.sh | sh
#
# Installs to ~/.local (binary on PATH, icons, and a .desktop launcher entry).
# Set JETTY_PREFIX=/usr/local and run with sudo for a system-wide install.
set -eu

REPO="bozdemir/JeTTY"
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

# --- verify checksum (best-effort; never hard-fails when tools/sums absent) ---
sums_url="https://github.com/$REPO/releases/download/$tag/SHA256SUMS.txt"
if curl -fsSL "$sums_url" -o "$tmp/SHA256SUMS.txt" 2>/dev/null; then
  want="$(sed -n "s/^\\([0-9a-f]\\{64\\}\\)  *$asset\$/\\1/p" "$tmp/SHA256SUMS.txt")"
  if [ -n "$want" ]; then
    if command -v sha256sum >/dev/null 2>&1; then
      got="$(sha256sum "$tmp/jetty.tar.gz" | cut -d' ' -f1)"
    elif command -v shasum >/dev/null 2>&1; then
      got="$(shasum -a 256 "$tmp/jetty.tar.gz" | cut -d' ' -f1)"
    fi
    if [ -n "${got:-}" ] && [ "$got" != "$want" ]; then
      die "checksum mismatch for $asset (expected $want, got $got) — aborting"
    fi
    [ -n "${got:-}" ] && say "Checksum verified."
  fi
fi

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

# Absolute Exec= so the launcher entry works even when $PREFIX/bin is off the
# session PATH (common for ~/.local installs without PATH update). The path is
# quoted/escaped per the Desktop Entry spec (double-quoted; \ " ` $ escaped;
# % doubled to %%) — matching the app's autostart Exec — so a $PREFIX with a
# space or reserved char yields a working launcher instead of a silent no-op.
desktop="$PREFIX/share/applications/jetty.desktop"
if [ -f "$desktop" ]; then
  esc="$(printf '%s' "$PREFIX/bin/jetty" \
    | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e 's/`/\\`/g' -e 's/\$/\\$/g' -e 's/%/%%/g')"
  # Rewrite via awk (ENVIRON, no escape processing) so reserved chars in the
  # replacement never corrupt a sed program/replacement.
  NEWEXEC="Exec=\"$esc\"" awk \
    '/^Exec=jetty/ { print ENVIRON["NEWEXEC"] substr($0, length("Exec=jetty") + 1); next } { print }' \
    "$desktop" > "$desktop.tmp" && mv "$desktop.tmp" "$desktop"
fi

gtk-update-icon-cache "$PREFIX/share/icons/hicolor" >/dev/null 2>&1 || true
update-desktop-database "$PREFIX/share/applications" >/dev/null 2>&1 || true

say "JeTTY $tag installed → $PREFIX/bin/jetty"
case ":$PATH:" in
  *":$PREFIX/bin:"*) ;;
  *) printf '\033[1;33mnote:\033[0m add %s to your PATH:\n  export PATH="%s:$PATH"\n' "$PREFIX/bin" "$PREFIX/bin" ;;
esac
printf 'Launch it with: \033[1;36mjetty\033[0m   (press F9 to summon)\n'
