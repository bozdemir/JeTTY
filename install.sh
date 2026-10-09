#!/bin/sh
# JeTTY one-line installer — downloads the latest prebuilt release (no Rust
# toolchain needed) and installs it for the current user.
#
#   curl -fsSL https://raw.githubusercontent.com/bozdemir/JeTTY/main/install.sh | sh
#
# Installs to ~/.local (binary on PATH, icons, and a .desktop launcher entry).
# Set JETTY_PREFIX=/usr/local and run with sudo for a system-wide install.
#
# The download is verified against the release's SHA256SUMS.txt and the install
# ABORTS if that can't be done (no sums file, no line for the archive, no sha256
# tool). JETTY_INSECURE_SKIP_VERIFY=1 skips verification — only for a mirror you
# trust some other way.
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
# Follow github.com's /releases/latest redirect (…/releases/tag/vX.Y.Z): unlike
# the REST API it has no 60-requests-per-hour anonymous rate limit. The API is
# only a fallback.
say "Finding the latest JeTTY release…"
latest="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest" 2>/dev/null || true)"
tag="${latest##*/tag/}"
case "$tag" in
  v[0-9]*) ;;
  *) tag="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null \
             | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)" ;;
esac
case "$tag" in
  v[0-9]*) ;;
  *) die "could not find a release. See https://github.com/$REPO/releases" ;;
esac
ver="${tag#v}"

asset="jetty-${ver}-x86_64-linux.tar.gz"
url="https://github.com/$REPO/releases/download/$tag/$asset"

# --- download + extract ---
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
say "Downloading $asset…"
curl -fSL "$url" -o "$tmp/jetty.tar.gz" || die "download failed: $url"

# --- verify checksum (mandatory) ---
# Every way verification could silently not happen is an abort: a missing sums
# file, a sums file without a line for this archive, or no sha256 tool.
if [ "${JETTY_INSECURE_SKIP_VERIFY:-0}" = "1" ]; then
  printf '\033[1;33mwarning:\033[0m skipping checksum verification (JETTY_INSECURE_SKIP_VERIFY=1)\n' >&2
else
  sums_url="https://github.com/$REPO/releases/download/$tag/SHA256SUMS.txt"
  curl -fsSL "$sums_url" -o "$tmp/SHA256SUMS.txt" \
    || die "could not download $sums_url to verify the download — aborting"
  want="$(sed -n "s/^\\([0-9a-f]\\{64\\}\\)  *$asset\$/\\1/p" "$tmp/SHA256SUMS.txt" | head -n1)"
  [ -n "$want" ] || die "SHA256SUMS.txt has no checksum for $asset — aborting"
  if command -v sha256sum >/dev/null 2>&1; then
    got="$(sha256sum "$tmp/jetty.tar.gz" | cut -d' ' -f1)"
  elif command -v shasum >/dev/null 2>&1; then
    got="$(shasum -a 256 "$tmp/jetty.tar.gz" | cut -d' ' -f1)"
  else
    die "no sha256sum or shasum found to verify the download — install one (coreutils / perl) and retry"
  fi
  [ "$got" = "$want" ] || die "checksum mismatch for $asset (expected $want, got $got) — aborting"
  say "Checksum verified."
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
# quoted/escaped per the Desktop Entry spec — matching the app's autostart Exec
# (`desktop_exec_arg`): pass 1 double-quotes it with \ " ` $ backslash-escaped,
# pass 2 (the spec's general string escape, applied AFTER quoting) doubles every
# backslash, then % is doubled to %%. Without pass 2 a $PREFIX containing one of
# those chars produced `\$`, which GLib rejects as an invalid escape — a launcher
# that silently does nothing.
desktop="$PREFIX/share/applications/jetty.desktop"
if [ -f "$desktop" ]; then
  esc="$(printf '%s' "$PREFIX/bin/jetty" \
    | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e 's/`/\\`/g' -e 's/\$/\\$/g' \
          -e 's/\\/\\\\/g' -e 's/%/%%/g')"
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
# On Wayland JeTTY grabs no key: the compositor's shortcut summons it.
if [ -n "${WAYLAND_DISPLAY:-}" ]; then
  printf 'Launch it with: \033[1;36mjetty\033[0m   (to summon it from anywhere, bind \033[1;36mjetty --toggle\033[0m to a key in your compositor)\n'
else
  printf 'Launch it with: \033[1;36mjetty\033[0m   (press F9 to summon)\n'
fi
