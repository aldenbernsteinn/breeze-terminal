#!/usr/bin/env bash
# Build Breeze.app — a macOS app bundle so the Dock/Finder shows the app icon.
# A bare `cargo` binary has no icon; macOS only uses one from a .app bundle.
#
#   ./scripts/make-app.sh            # builds ./Breeze.app
#   ./scripts/make-app.sh /Applications   # also installs it there
set -euo pipefail
cd "$(dirname "$0")/.."   # repo root

APP="Breeze"
APP_DIR="$PWD/$APP.app"
CONTENTS="$APP_DIR/Contents"

echo "▸ Building release…"
cargo build --release
BIN="target/release/breeze"
[[ -f "$BIN" ]] || { echo "build failed — $BIN not found"; exit 1; }

echo "▸ Assembling $APP.app…"
rm -rf "$APP_DIR"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"
cp "$BIN" "$CONTENTS/MacOS/breeze"
chmod +x "$CONTENTS/MacOS/breeze"
cp Resources/Info.plist "$CONTENTS/Info.plist"
cp Resources/AppIcon.icns "$CONTENTS/Resources/AppIcon.icns"

echo "▸ Ad-hoc codesigning (free; avoids the 'damaged' Gatekeeper error)…"
codesign --force --deep -s - "$APP_DIR" >/dev/null 2>&1 || true

DEST="${1-}"
if [[ -n "$DEST" ]]; then
  echo "> Installing to ${DEST}..."
  rm -rf "$DEST/$APP.app"
  cp -R "$APP_DIR" "$DEST/"
  echo "✓ $DEST/$APP.app"
else
  echo "✓ $APP_DIR"
fi
