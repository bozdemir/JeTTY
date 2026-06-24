#!/usr/bin/env bash
# Install JeTTY for the current user: binary on PATH, app icon, and a desktop
# entry so it shows up in your launcher and Alt-Tab with the JeTTY icon.
set -e
cd "$(dirname "$0")/.."

echo "Building release binary…"
cargo build --release --bin jetty

mkdir -p ~/.local/bin ~/.local/share/applications
install -m755 target/release/jetty ~/.local/bin/jetty

for sz in 16 32 48 64 128 256; do
  dir=~/.local/share/icons/hicolor/${sz}x${sz}/apps
  mkdir -p "$dir"
  install -m644 "assets/icons/jetty-${sz}.png" "$dir/jetty.png"
done

install -m644 assets/jetty.desktop ~/.local/share/applications/jetty.desktop

gtk-update-icon-cache ~/.local/share/icons/hicolor 2>/dev/null || true
update-desktop-database ~/.local/share/applications 2>/dev/null || true

echo "Installed JeTTY. Ensure ~/.local/bin is on your PATH, then launch 'jetty'"
echo "or find JeTTY in your application launcher."
