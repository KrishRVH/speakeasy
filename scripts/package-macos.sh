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
signing_identity=${SPEAKEASY_MAC_SIGN_IDENTITY:-}
notary_profile=${SPEAKEASY_MAC_NOTARY_PROFILE:-}
if [[ -n "$signing_identity" || -n "$notary_profile" ]]; then
  if [[ -z "$signing_identity" || "$signing_identity" == - || -z "$notary_profile" ]]; then
    echo "Distribution needs both SPEAKEASY_MAC_SIGN_IDENTITY and SPEAKEASY_MAC_NOTARY_PROFILE. Use a Developer ID Application identity and a stored notarytool keychain profile." >&2
    exit 1
  fi
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
cp LICENSE "$bundle/Contents/Resources/"
cargo about generate --locked --fail -c packaging/about.toml --target aarch64-apple-darwin -o "$bundle/Contents/Resources/THIRD-PARTY-NOTICES.txt" packaging/notices.hbs
cp crates/app/assets/JosefinSans-OFL.txt "$bundle/Contents/Resources/"
cp crates/show/NOTICE.md "$bundle/Contents/Resources/"
# An ad-hoc signature's designated requirement is the binary's cdhash, so every build drops the
# Accessibility and Screen Recording grants. A stable bundle identifier is necessary but not
# sufficient: only a persistent signing identity keeps grants, and its requirement includes the
# identifier, so never rename CFBundleIdentifier. Gatekeeper also wants notarization.
if [[ -n "$signing_identity" ]]; then
  codesign --force --sign "$signing_identity" --options runtime --timestamp \
    --entitlements packaging/macos/Entitlements.plist "$bundle"
  codesign --verify --strict "$bundle"
  # Notarize a temporary upload; the distribution ZIP appears only after acceptance and stapling.
  notary_directory=$(mktemp -d artifacts/.macos-notary.XXXXXX)
  trap 'rm -rf -- "$notary_directory"' EXIT
  ditto -c -k --keepParent "$bundle" "$notary_directory/upload.zip"
  xcrun notarytool submit "$notary_directory/upload.zip" --keychain-profile "$notary_profile" \
    --wait --output-format json > "$notary_directory/result.json"
  if ! python3 -c '
import json, sys
result = json.load(sys.stdin)
if result.get("status") != "Accepted":
    print("Notarization submission:", result.get("id", "unavailable"), file=sys.stderr)
    sys.exit(1)
' < "$notary_directory/result.json"; then
    echo "Apple did not accept notarization. Check this submission with notarytool before distributing." >&2
    exit 1
  fi
  xcrun stapler staple "$bundle"
  xcrun stapler validate "$bundle"
  spctl --assess --type execute "$bundle"
else
  codesign --force --sign - "$bundle"
  codesign --verify --strict "$bundle"
fi
ditto -c -k --keepParent "$bundle" artifacts/speakeasy-macos.zip
# Keep the ZIP for direct downloads and create the familiar drag-to-Applications installation.
dmg_directory=$(mktemp -d artifacts/.macos-dmg.XXXXXX)
trap 'rm -rf -- "$dmg_directory" "${notary_directory:-}"' EXIT
ditto "$bundle" "$dmg_directory/Speakeasy.app"
ln -s /Applications "$dmg_directory/Applications"
hdiutil create -quiet -ov -format UDZO -volname Speakeasy -srcfolder "$dmg_directory" artifacts/speakeasy-macos.dmg
if [[ -n "$signing_identity" ]]; then
  codesign --sign "$signing_identity" --timestamp artifacts/speakeasy-macos.dmg
  xcrun notarytool submit artifacts/speakeasy-macos.dmg --keychain-profile "$notary_profile" \
    --wait --output-format json > "$dmg_directory/result.json"
  python3 -c 'import json,sys; sys.exit(0 if json.load(sys.stdin).get("status") == "Accepted" else "Disk image notarization was rejected.")' < "$dmg_directory/result.json"
  xcrun stapler staple artifacts/speakeasy-macos.dmg
  xcrun stapler validate artifacts/speakeasy-macos.dmg
  spctl --assess --type open --context context:primary-signature artifacts/speakeasy-macos.dmg
fi
echo "Built $bundle"
