#!/usr/bin/env bash
# Launches a release package the way people and agents first meet it, without a display, model,
# or microphone: its version and help, a mistyped option, and the scripted Show.
set -euo pipefail
if [[ $# -ne 1 || ! -f "$1" ]]; then
  echo "Usage: bash scripts/check-package.sh <package or executable file>" >&2
  exit 1
fi
manifest=$(dirname -- "$0")/../Cargo.toml
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$manifest")
if [[ -z "$version" ]]; then
  echo "$manifest names no workspace version" >&2
  exit 1
fi
package=$(realpath -- "$1")
work=$(mktemp -d)
mounted=
cleanup() {
  if [[ -n "$mounted" ]]; then
    hdiutil detach -quiet "$mounted" || echo "Could not detach $mounted" >&2
  fi
  rm -rf -- "$work"
}
trap cleanup EXIT
fail() {
  echo "$package: $*" >&2
  exit 1
}
# The Show goes to a relative path, which a Windows executable resolves correctly whether Git Bash
# or WSL launched it; each would rewrite or misplace an absolute POSIX path.
cd "$work"
case "$package" in
  *-windows-x64.zip)
    unzip -q "$package" || fail "cannot be unzipped"
    app=./speakeasy-cli.exe
    ;;
  *-macos.zip)
    unzip -q "$package" || fail "cannot be unzipped"
    app=./Speakeasy.app/Contents/MacOS/speakeasy
    ;;
  *-macos.dmg)
    hdiutil attach -quiet -readonly -nobrowse -mountpoint "$work/mounted" "$package" || fail "cannot be mounted"
    mounted="$work/mounted"
    [[ -L "$mounted/Applications" && "$(readlink "$mounted/Applications")" == /Applications ]] || fail "has no Applications link"
    ditto "$mounted/Speakeasy.app" "$work/Speakeasy.app" || fail "cannot be copied to a writable folder"
    app=./Speakeasy.app/Contents/MacOS/speakeasy
    ;;
  *.tar.gz)
    tar -xzf "$package" || fail "cannot be extracted"
    app=./Speakeasy.AppDir/AppRun
    ;;
  *)
    app=$package
    # An AppImage unpacks itself rather than mounting through FUSE, which a runner may lack.
    export APPIMAGE_EXTRACT_AND_RUN=1
    ;;
esac
[[ -f "$app" ]] || fail "holds no ${app#./}"
printed=$("$app" --version) || fail "--version exited $?"
[[ "$printed" == "speakeasy $version" ]] || fail "--version printed '$printed', not 'speakeasy $version'"
help=$("$app" --help) || fail "--help exited $?"
[[ -n "$help" ]] || fail "--help printed nothing"
status=0
"$app" --no-such-option 2> rejected.txt || status=$?
[[ $status -eq 2 ]] || fail "a mistyped option exited $status, not the usage-error status 2"
[[ -s rejected.txt ]] || fail "a mistyped option gave no reason on stderr"
"$app" show demo --out show > /dev/null || fail "show demo exited $?"
[[ -s show/summary.md ]] || fail "show demo wrote no summary.md"
[[ -s show/contact-sheet.png ]] || fail "show demo wrote no contact-sheet.png"
"$app" show inspect show --json > inspection.json || fail "show inspect exited $?"
[[ -s inspection.json ]] || fail "show inspect printed nothing"
"$app" show skill > SKILL.md || fail "show skill exited $?"
[[ -s SKILL.md ]] || fail "show skill printed nothing"
rm show/frames/frame-01.png
status=0
"$app" show inspect show --json > damaged.json 2> rejected.txt || status=$?
[[ $status -eq 1 ]] || fail "a damaged Show exited $status, not status 1"
[[ -s damaged.json && -s rejected.txt ]] || fail "a damaged Show gave no findings or failure"
echo "Launched $package"
