#!/usr/bin/env bash
# Launches a release package the way people and agents first meet it, without a display, model,
# or microphone: its version and help, a mistyped option, and the scripted Show.
set -euo pipefail
if [[ $# -ne 1 ]]; then
  echo "Usage: bash scripts/check-package.sh <package or executable>" >&2
  exit 1
fi
package=$(realpath -- "$1")
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$(dirname -- "$0")/../Cargo.toml")
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
fail() {
  echo "$package: $*" >&2
  exit 1
}
# The Show goes to a relative path, which a Windows executable resolves correctly whether Git Bash
# or WSL launched it; each would rewrite or misplace an absolute POSIX path.
cd "$work"
case "$package" in
  *-windows-x64.zip)
    unzip -q "$package"
    app=./speakeasy.exe
    ;;
  *-macos.zip)
    unzip -q "$package"
    app=./Speakeasy.app/Contents/MacOS/speakeasy
    ;;
  *.tar.gz)
    tar -xzf "$package"
    app=./Speakeasy.AppDir/AppRun
    ;;
  *)
    app=$package
    # An AppImage unpacks itself rather than mounting through FUSE, which a runner may lack.
    export APPIMAGE_EXTRACT_AND_RUN=1
    ;;
esac
printed=$("$app" --version)
[[ "$printed" == "speakeasy $version" ]] || fail "--version printed '$printed', not 'speakeasy $version'"
help=$("$app" --help)
[[ -n "$help" ]] || fail "--help printed nothing"
status=0
"$app" --no-such-option 2> rejected.txt || status=$?
[[ $status -eq 2 && -s rejected.txt ]] || fail "a mistyped option exited $status without a reason on stderr"
"$app" show demo --out show > /dev/null
[[ -s show/summary.md && -s show/contact-sheet.png ]] || fail "show demo wrote no summary.md or contact sheet"
echo "Launched $package"
