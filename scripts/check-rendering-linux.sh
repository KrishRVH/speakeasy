#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
case "${1:-}" in
    --packages|'') ;;
    *) echo "Usage: bash scripts/check-rendering-linux.sh [--packages]" >&2; exit 1 ;;
esac

test_binary=$(cargo test --locked -p speakeasy --bin speakeasy --no-run --message-format=json | python3 -c '
import json, sys
for line in sys.stdin:
    event = json.loads(line)
    if (event.get("reason") == "compiler-artifact"
            and event["target"]["name"] == "speakeasy"
            and event["profile"]["test"] and event.get("executable")):
        print(event["executable"])
')
test -x "$test_binary"
cargo build --locked -p speakeasy --example check_native_rendering

# Keep the builder ABI while using a stock driver with Mesa's X11 PutImage fix.
# The read-only mount runs the builder's exact binaries, with no host display.
docker run --rm --volume "$PWD:$PWD:ro" --workdir "$PWD" ubuntu:24.04 \
    bash -euo pipefail -c '
        apt-get update -qq
        apt-get install -y -qq --no-install-recommends \
            libasound2t64 libfontconfig1 libfreetype6 libx11-6 libxcb1 \
            libxkbcommon0 libxkbcommon-x11-0 libvulkan1 libssl3t64 \
            mesa-vulkan-drivers xvfb xauth x11-utils python3
        dpkg-query -W mesa-vulkan-drivers libvulkan1
        icd=(/usr/share/vulkan/icd.d/lvp_icd*.json)
        test -f "${icd[0]}"
        export VK_ICD_FILENAMES="${icd[0]}" LP_NUM_THREADS=4 XDG_SESSION_TYPE=x11
        for scale in 1 2; do
            GPUI_X11_SCALE_FACTOR="$scale" xvfb-run -a -s "-screen 0 1280x720x24" \
                "$1" idle_native_pill_ --ignored --test-threads=1
        done
        for scale in 1 1.25 1.5 2; do
            GPUI_X11_SCALE_FACTOR="$scale" xvfb-run -a -s "-screen 0 1280x720x24" \
                target/debug/examples/check_native_rendering --native-gui
        done
        if [[ "$2" == --packages ]]; then
            export APPIMAGE_EXTRACT_AND_RUN=1
            artifacts/speakeasy-linux-x86_64.AppImage --help
            python3 scripts/check-demo-linux.py artifacts/speakeasy-linux-x86_64.AppImage
            directory=$(mktemp -d)
            trap '\''rm -rf -- "$directory"'\'' EXIT
            tar -xzf artifacts/speakeasy-linux-x86_64.tar.gz -C "$directory"
            "$directory/Speakeasy.AppDir/AppRun" --help
            python3 scripts/check-demo-linux.py "$directory/Speakeasy.AppDir/AppRun" --startup-only
        else
            python3 scripts/check-demo-linux.py target/release/speakeasy
        fi
    ' bash "$test_binary" "${1:-}"
