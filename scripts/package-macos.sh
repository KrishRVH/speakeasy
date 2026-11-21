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
mkdir -p artifacts
# One run at a time owns the outputs. The lock belongs to this shell's open descriptor, so it ends
# with the run even when the run is killed.
exec 9>> artifacts/.package-macos.lock
python3 -c '
import fcntl, sys
try:
    fcntl.flock(9, fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError:
    sys.exit("Another macOS package run is using artifacts. Wait for it to finish, then try again.")
'
rm -rf -- artifacts/Speakeasy.app
rm -f -- artifacts/speakeasy-macos.zip artifacts/speakeasy-macos.dmg
signing_identity=${SPEAKEASY_MAC_SIGN_IDENTITY:-}
notary_profile=${SPEAKEASY_MAC_NOTARY_PROFILE:-}
if [[ -n "$signing_identity" || -n "$notary_profile" ]]; then
  if [[ -z "$signing_identity" || "$signing_identity" == - || -z "$notary_profile" ]]; then
    echo "Distribution needs both SPEAKEASY_MAC_SIGN_IDENTITY and SPEAKEASY_MAC_NOTARY_PROFILE. Use a Developer ID Application identity and a stored notarytool keychain profile." >&2
    exit 1
  fi
fi
staging=$(mktemp -d artifacts/.macos-package.XXXXXX)
outputs=(Speakeasy.app speakeasy-macos.zip speakeasy-macos.dmg)
publishing=
# An output leaves staging only through its rename into artifacts. Only a run that exits 0 keeps
# what it published; any other exit once publication starts takes back exactly the outputs missing
# from staging. Signals exit through this trap too, and it ignores them while it cleans up.
discard() {
  local status=$?
  trap '' HUP INT PIPE TERM
  if [[ $status -ne 0 && -n "$publishing" ]]; then
    for output in "${outputs[@]}"; do
      if [[ ! -e "$staging/$output" ]]; then
        rm -rf -- "artifacts/$output"
      fi
    done
  fi
  rm -rf -- "$staging"
}
trap discard EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 141' PIPE
trap 'exit 143' TERM
# Submits one archive and stops unless Apple accepts it, naming the submission to check.
notarize() {
  xcrun notarytool submit "$1" --keychain-profile "$notary_profile" --wait --output-format json > "$staging/notarization.json"
  python3 -c '
import json, sys
result = json.load(sys.stdin)
if result.get("status") != "Accepted":
    submission = result.get("id", "unavailable")
    sys.exit(f"Apple did not accept notarization of the {sys.argv[1]}. Check submission {submission} with notarytool before distributing.")
' "$2" < "$staging/notarization.json"
}
MACOSX_DEPLOYMENT_TARGET=14.0 cargo build --release --locked -p speakeasy
bundle="$staging/Speakeasy.app"
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
# Ad-hoc signatures change identity with the binary and can require permission grants again.
# A persistent signing identity and bundle identifier support permission continuity; verify
# consent after replacement. Gatekeeper also requires notarization for distribution.
if [[ -n "$signing_identity" ]]; then
  codesign --force --sign "$signing_identity" --options runtime --timestamp \
    --entitlements packaging/macos/Entitlements.plist "$bundle"
  codesign --verify --strict "$bundle"
  ditto -c -k --keepParent "$bundle" "$staging/upload.zip"
  notarize "$staging/upload.zip" app
  xcrun stapler staple "$bundle"
  xcrun stapler validate "$bundle"
  spctl --assess --type execute "$bundle"
else
  codesign --force --sign - "$bundle"
  codesign --verify --strict "$bundle"
fi
ditto -c -k --keepParent "$bundle" "$staging/speakeasy-macos.zip"
dmg_directory="$staging/dmg"
mkdir "$dmg_directory"
ditto "$bundle" "$dmg_directory/Speakeasy.app"
ln -s /Applications "$dmg_directory/Applications"
hdiutil create -quiet -ov -format UDZO -volname Speakeasy -srcfolder "$dmg_directory" "$staging/speakeasy-macos.dmg"
if [[ -n "$signing_identity" ]]; then
  codesign --sign "$signing_identity" --timestamp "$staging/speakeasy-macos.dmg"
  notarize "$staging/speakeasy-macos.dmg" "disk image"
  xcrun stapler staple "$staging/speakeasy-macos.dmg"
  xcrun stapler validate "$staging/speakeasy-macos.dmg"
  spctl --assess --type open --context context:primary-signature "$staging/speakeasy-macos.dmg"
fi
# The app goes first, so an archive never appears without it.
publishing=yes
for output in "${outputs[@]}"; do
  mv "$staging/$output" artifacts/
done
echo "Built artifacts/Speakeasy.app"
