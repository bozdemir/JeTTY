#!/bin/sh
# Package the JeTTY release binary into a macOS .app bundle so it gets a proper
# Dock / Launchpad icon (winit's window icon is a no-op on macOS — the icon must
# come from the bundle's Info.plist + .icns). Run on macOS AFTER building:
#
#   cargo build --release && sh scripts/make-macos-app.sh
#   open JeTTY.app          # or: cp -r JeTTY.app /Applications/
#
# Uses sips + iconutil, both built into macOS.
set -e
cd "$(dirname "$0")/.."

command -v sips >/dev/null 2>&1 || { echo "error: sips not found (macOS only)"; exit 1; }
command -v iconutil >/dev/null 2>&1 || { echo "error: iconutil not found (macOS only)"; exit 1; }
[ -f assets/icons/jetty-256.png ] || { echo "error: assets/icons/jetty-256.png missing"; exit 1; }

VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"
[ -n "$VERSION" ] || { echo "error: could not parse version from Cargo.toml"; exit 1; }

BIN=target/release/jetty
[ -x "$BIN" ] || { echo "building release binary…"; cargo build --release --bin jetty; }

APP="JeTTY.app"
echo "Assembling $APP…"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/jetty"

# Build Icon.icns from the embedded 256px PNG (sips upsamples the retina sizes).
SRC=assets/icons/jetty-256.png
ICONSET="$(mktemp -d)/jetty.iconset"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  sips -z "$s" "$s"     "$SRC" --out "$ICONSET/icon_${s}x${s}.png"    >/dev/null
  d=$((s * 2))
  sips -z "$d" "$d"     "$SRC" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/jetty.icns"
[ -f "$APP/Contents/Resources/jetty.icns" ] || { echo "error: .icns not generated"; exit 1; }

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>JeTTY</string>
  <key>CFBundleDisplayName</key><string>JeTTY</string>
  <key>CFBundleIdentifier</key><string>com.bozdemir.jetty</string>
  <key>CFBundleExecutable</key><string>jetty</string>
  <key>CFBundleIconFile</key><string>jetty</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>10.13</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

echo "Done → $APP"
echo "  Run:     open $APP"
echo "  Install: cp -r $APP /Applications/"
