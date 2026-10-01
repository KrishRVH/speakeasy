#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ $# -ne 0 ]]; then
  echo "Usage: bash scripts/package-macos.sh" >&2
  exit 1
fi
if [[ "$(uname -s)" != Darwin ]]; then
  echo "Run this on macOS with the Xcode command line tools installed." >&2
  exit 1
fi
MACOSX_DEPLOYMENT_TARGET=14.0 cargo build --release --locked -p speakeasy
bundle="artifacts/Speakeasy.app"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp target/release/speakeasy "$bundle/Contents/MacOS/speakeasy"
cp packaging/macos/Info.plist "$bundle/Contents/Info.plist"
cp packaging/macos/Speakeasy.icns "$bundle/Contents/Resources/"
cp settings.example.json "$bundle/Contents/Resources/"
cp crates/app/assets/JosefinSans-OFL.txt "$bundle/Contents/Resources/"
# Local development signature; distribution still requires Developer ID signing
# and notarization. Keep the bundle identifier stable for OS permissions.
codesign --force --sign - "$bundle"
ditto -c -k --keepParent "$bundle" artifacts/speakeasy-macos.zip
echo "Built $bundle"
