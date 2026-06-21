#!/usr/bin/env bash
# Build Breeze.dmg — a distributable disk image wrapping Breeze.app.
#
#   ./scripts/make-dmg.sh   # builds ./Breeze.app then ./Breeze.dmg
set -euo pipefail
cd "$(dirname "$0")/.."

./scripts/make-app.sh

echo "▸ Packaging Breeze.dmg…"
rm -f Breeze.dmg
hdiutil create \
  -volname Breeze \
  -srcfolder Breeze.app \
  -ov -format UDZO \
  Breeze.dmg >/dev/null

echo "✓ $PWD/Breeze.dmg"
