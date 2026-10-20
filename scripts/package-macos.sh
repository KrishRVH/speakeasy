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
rm -rf "$bundle" artifacts/speakeasy-macos.zip
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp target/release/speakeasy "$bundle/Contents/MacOS/speakeasy"
cp packaging/macos/Info.plist "$bundle/Contents/Info.plist"
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml)
if [[ ! "$version" =~ ^([0-9]+)\.([0-9]{1,2})\.([0-9]{1,2})$ ]]; then
  echo "Cannot derive bundle versions from the workspace version in Cargo.toml." >&2
  exit 1
fi
# LaunchServices prefers the copy with the highest CFBundleVersion, so it must keep rising;
# two-digit minor and patch numbers keep this encoding monotonic.
build=$((BASH_REMATCH[1] * 10000 + BASH_REMATCH[2] * 100 + BASH_REMATCH[3]))
# -insert fails if the template defines either key again, so Cargo.toml stays the only version source.
plutil -insert CFBundleShortVersionString -string "$version" "$bundle/Contents/Info.plist"
plutil -insert CFBundleVersion -string "$build" "$bundle/Contents/Info.plist"
cp packaging/macos/Speakeasy.icns "$bundle/Contents/Resources/"
cp settings.example.json "$bundle/Contents/Resources/"
cp crates/app/assets/JosefinSans-OFL.txt "$bundle/Contents/Resources/"
cp crates/show/NOTICE.md "$bundle/Contents/Resources/"
# An ad-hoc signature's designated requirement is the binary's cdhash, so every build drops the
# Accessibility and Screen Recording grants; a stable bundle identifier does not prevent this.
# Only a persistent signing identity keeps grants, and Gatekeeper also wants notarization.
codesign --force --sign - "$bundle"
ditto -c -k --keepParent "$bundle" artifacts/speakeasy-macos.zip
echo "Built $bundle"
