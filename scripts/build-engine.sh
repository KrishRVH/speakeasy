#!/usr/bin/env bash
# Builds NeMo-Speech.cpp v0.1.0 with Speakeasy's patches for Apple silicon on macOS 14 or later
# and stages an installation laid out like NVIDIA's release: <name>/nemo-speech/{bin,lib,share}.
# The name carries a hash of every input, so setup can tell builds apart.
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ $# -gt 1 ]]; then
  echo "Usage: bash scripts/build-engine.sh [STAGING_DIRECTORY]" >&2
  exit 1
fi
if [[ "$(uname -s)" != Darwin || "$(uname -m)" != arm64 ]]; then
  echo "Build the engine on an Apple silicon Mac with Xcode and its Metal toolchain." >&2
  exit 1
fi
staging="${1:-artifacts/engine}"
work="artifacts/engine-build"
nemo_commit=4f9676226f667d14608487df744f375db87127f8
ggml_commit=c03b4e2bcece5134827881af90242086daf75be5
sentencepiece_commit=17d7580d6407802f85855d2cc9190634e2c95624
httplib_commit=62d899feac3cf9215a55f2b43da250fdd98d2156
# M1 is the oldest supported Mac; never tune the CPU code for the build machine.
cpu_arch=armv8.5-a+fp16+dotprod
deployment=14.0

key="$(
  {
    printf '%s\n' "$nemo_commit" "$ggml_commit" "$sentencepiece_commit" "$httplib_commit" \
      "$cpu_arch" "$deployment"
    cat scripts/build-engine.sh packaging/engine/nemo-speech.patch packaging/engine/ggml.patch
  } | shasum -a 256 | cut -c1-12
)"
name="nemo-speech-0.1.0-speakeasy-$key"
if [[ -x "$staging/$name/nemo-speech/bin/nemo-speech" ]]; then
  echo "$staging/$name"
  exit 0
fi

# Keeps tool output out of the way unless a step fails.
quiet() {
  if ! "$@" > "$work/step.log" 2>&1; then
    tail -n 40 "$work/step.log" >&2
    exit 1
  fi
}

checkout() {
  local directory=$1 url=$2 commit=$3
  if [[ ! -d "$directory/.git" ]]; then
    mkdir -p "$directory"
    git -C "$directory" init -q
    git -C "$directory" remote add origin "$url"
  fi
  git -C "$directory" fetch -q --depth 1 origin "$commit"
  git -C "$directory" checkout -q --force "$commit"
  git -C "$directory" clean -qfdx
}
checkout "$work/nemo" https://github.com/NVIDIA/NeMo-Speech.cpp.git "$nemo_commit"
checkout "$work/nemo/ggml" https://github.com/ggml-org/ggml.git "$ggml_commit"
checkout "$work/nemo/third_party/cpp-httplib" https://github.com/yhirose/cpp-httplib.git "$httplib_commit"
checkout "$work/sentencepiece" https://github.com/google/sentencepiece.git "$sentencepiece_commit"
git -C "$work/nemo" apply "$PWD/packaging/engine/nemo-speech.patch"
git -C "$work/nemo/ggml" apply "$PWD/packaging/engine/ggml.patch"

common=(
  -DCMAKE_BUILD_TYPE=Release
  -DCMAKE_OSX_ARCHITECTURES=arm64
  -DCMAKE_OSX_DEPLOYMENT_TARGET="$deployment"
  # SentencePiece predates CMake 4's minimum policy version.
  -DCMAKE_POLICY_VERSION_MINIMUM=3.5
)
quiet cmake "${common[@]}" -S "$work/sentencepiece" -B "$work/sentencepiece-build" \
  -DSPM_BUILD_TEST=OFF -DSPM_ENABLE_SHARED=OFF -DSPM_ENABLE_TCMALLOC=OFF
quiet cmake --build "$work/sentencepiece-build" --parallel --target sentencepiece-static

quiet cmake "${common[@]}" -S "$work/nemo" -B "$work/build" \
  -DNEMO_SPEECH_GGML_PATCHED=OFF \
  -DNEMO_SPEECH_BUILD_ASR=ON \
  -DNEMO_SPEECH_BUILD_DIAR=OFF \
  -DNEMO_SPEECH_BUILD_TTS=OFF \
  -DNEMO_SPEECH_BUILD_NMT=OFF \
  -DNEMO_SPEECH_BUILD_HTTP=ON \
  -DNEMO_SPEECH_BUILD_MIC_CAPTURE=OFF \
  -DNEMO_SPEECH_BUILD_EXAMPLES=OFF \
  -DNEMO_SPEECH_BUILD_TESTS=OFF \
  -DNEMO_SPEECH_BUILD_TOOLS=OFF \
  -DGGML_METAL=ON \
  -DGGML_METAL_EMBED_LIBRARY=ON \
  -DGGML_METAL_EMBED_COMPILED=ON \
  -DGGML_METAL_MACOSX_VERSION_MIN="$deployment" \
  -DGGML_NATIVE=OFF \
  -DGGML_CPU_ARM_ARCH="$cpu_arch" \
  -DSENTENCEPIECE_LIB="$PWD/$work/sentencepiece-build/src/libsentencepiece.a" \
  -DSENTENCEPIECE_INCLUDE_DIR="$PWD/$work/sentencepiece/src"
quiet cmake --build "$work/build" --parallel
rm -rf "$work/install"
quiet cmake --install "$work/build" --prefix "$work/install"

part="$staging/$name.part"
rm -rf "$part"
mkdir -p "$part/nemo-speech/lib"
cp -R "$work/install/bin" "$work/install/share" "$part/nemo-speech/"
cp -a "$work/install/lib/"*.dylib "$part/nemo-speech/lib/"
cp "$work/sentencepiece/LICENSE" "$part/nemo-speech/share/licenses/sentencepiece-LICENSE"

# Every Mach-O must load on the deployment floor and link only itself and the system.
while IFS= read -r binary; do
  minos="$(otool -l "$binary" | awk '/LC_BUILD_VERSION/ { found = 1 } found && $1 == "minos" { print $2; exit }')"
  if [[ "$minos" != "$deployment" ]]; then
    echo "$binary targets macOS $minos, not $deployment" >&2
    exit 1
  fi
  if otool -L "$binary" | tail -n +2 | awk '{ print $1 }' | grep -Ev '^(@rpath/|/usr/lib/|/System/Library/)'; then
    echo "$binary links a library outside the engine and the system" >&2
    exit 1
  fi
done < <(find "$part/nemo-speech/bin" "$part/nemo-speech/lib" -type f)

# The engine must load and find its accelerator here before it ships.
if ! "$part/nemo-speech/bin/nemo-speech" doctor --json > "$work/doctor.json"; then
  echo "The built engine failed its doctor check" >&2
  exit 1
fi

rm -rf "${staging:?}/$name"
mv "$part" "$staging/$name"
echo "$staging/$name"
