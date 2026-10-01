#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ $# -gt 1 ]]; then
  echo "Usage: bash scripts/package-linux.sh [--native-tar]" >&2
  exit 1
fi
if [[ "$(uname -s)" != Linux || "$(uname -m)" != x86_64 ]]; then
  echo "Linux packaging currently requires an x86_64 Linux builder." >&2
  exit 1
fi
native=false
case "${1:-}" in
  --native-tar) native=true ;;
  '') ;;
  *)
    echo "Usage: bash scripts/package-linux.sh [--native-tar]" >&2
    exit 1
    ;;
esac
cargo build --release --locked -p speakeasy
required=$(readelf --version-info target/release/speakeasy | rg -o 'GLIBC_[0-9.]+' | sort -Vu | tail -1)
if [[ "$native" == false && "$(printf '%s\n' GLIBC_2.35 "$required" | sort -V | tail -1)" != GLIBC_2.35 ]]; then
  echo "Binary requires $required. Build distribution packages on Ubuntu 22.04 (glibc 2.35), or use --native-tar for this machine." >&2
  exit 1
fi
mkdir -p artifacts
staging=$(mktemp -d "$PWD/artifacts/linux-build.XXXXXX")
trap 'rm -rf -- "$staging"' EXIT
app_dir="$staging/Speakeasy.AppDir"
install -Dm755 target/release/speakeasy "$app_dir/usr/bin/speakeasy"
install -Dm755 packaging/linux/AppRun "$app_dir/AppRun"
install -Dm644 packaging/linux/io.github.krvh.speakeasy.desktop "$app_dir/usr/share/applications/io.github.krvh.speakeasy.desktop"
install -Dm644 packaging/brand/app-icon.svg "$app_dir/usr/share/icons/hicolor/scalable/apps/io.github.krvh.speakeasy.svg"
install -Dm644 crates/app/assets/JosefinSans-OFL.txt "$app_dir/usr/share/licenses/speakeasy/JosefinSans-OFL.txt"
install -Dm644 settings.example.json "$app_dir/usr/share/speakeasy/settings.example.json"
{
  printf 'Architecture: x86_64\nApplication glibc requirement: %s\n' "$required"
  echo 'Rendering requires X11 or Xwayland and a working Vulkan driver.'
  echo 'Desktop portals and libei are host services, not bundled.'
  echo 'Speech engines and models are downloaded separately or selected in Settings.'
} > "$app_dir/BUILD.txt"
if [[ "$native" == true ]]; then
  tar -C "$staging" -czf artifacts/speakeasy-linux-x86_64-native.tar.gz Speakeasy.AppDir
  echo "Built a native tar archive for this machine; dependencies are not bundled."
  exit 0
fi
deploy="$staging/linuxdeploy.AppImage"
curl --fail --location --retry 3 --output "$deploy" 'https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage'
printf '%s  %s\n' c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d "$deploy" | sha256sum --check --status
chmod +x "$deploy"
export APPIMAGE_EXTRACT_AND_RUN=1
"$deploy" --appdir "$app_dir" --executable "$app_dir/usr/bin/speakeasy" --desktop-file "$app_dir/usr/share/applications/io.github.krvh.speakeasy.desktop" --icon-file "$app_dir/usr/share/icons/hicolor/scalable/apps/io.github.krvh.speakeasy.svg"
# Check bundled ELF dependencies as well as the app against the same ABI floor.
while IFS= read -r -d '' library; do
  versions=$(readelf --version-info "$library" 2> /dev/null | rg -o 'GLIBC_[0-9.]+' | sort -Vu | tail -1 || true)
  if [[ -n "$versions" && "$(printf '%s\n' GLIBC_2.35 "$versions" | sort -V | tail -1)" != GLIBC_2.35 ]]; then
    echo "Bundled library requires $versions: $library" >&2
    exit 1
  fi
done < <(rg --files --hidden -0 "$app_dir/usr/lib")
export OUTPUT="$PWD/artifacts/speakeasy-linux-x86_64.AppImage"
"$deploy" --appdir "$app_dir" --output appimage
tar -C "$staging" -czf artifacts/speakeasy-linux-x86_64.tar.gz Speakeasy.AppDir
sha256sum artifacts/speakeasy-linux-x86_64.AppImage artifacts/speakeasy-linux-x86_64.tar.gz > artifacts/speakeasy-linux-x86_64.sha256
echo "Built AppImage and bundled tar archive in artifacts/."
