#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ $(uname -s) != Darwin || $# -ne 0 ]]; then
  echo "Run this native movie check on macOS without arguments." >&2
  exit 1
fi
if ! command -v ffmpeg > /dev/null || ! command -v ffprobe > /dev/null; then
  echo "Install FFmpeg, which provides ffmpeg and ffprobe, to generate the synthetic movies." >&2
  exit 1
fi
mkdir -p artifacts
fixtures=$(mktemp -d artifacts/macos-movie.XXXXXX)
export SPEAKEASY_MOVIE_FIXTURES="$PWD/$fixtures"
python3 - "$fixtures" << 'PY'
import json
import pathlib
import subprocess
import sys

root = pathlib.Path(sys.argv[1])
flags = ["-an", "-c:v", "libx264", "-preset", "ultrafast", "-crf", "12", "-bf", "0", "-g", "30",
         "-pix_fmt", "yuv420p", "-color_primaries", "bt709", "-color_trc", "iec61966-2-1",
         "-colorspace", "smpte170m",
         "-bsf:v", "h264_metadata=colour_primaries=1:transfer_characteristics=13:matrix_coefficients=6",
         "-video_track_timescale", "30000", "-use_editlist", "0",
         "-movflags", "empty_moov+frag_keyframe+default_base_moof+skip_trailer"]
for name, replaced in [("moving", False), ("other", True)]:
    command = ["ffmpeg", "-v", "error", "-f", "rawvideo", "-pixel_format", "bgra", "-video_size", "320x240",
               "-framerate", "30000/1001", "-i", "pipe:0", *flags, str(root / f"{name}.mp4")]
    with subprocess.Popen(command, stdin=subprocess.PIPE) as process:
        assert process.stdin is not None
        for index in range(120):
            frame = bytearray()
            for y in range(240):
                for x in range(320):
                    if 100 <= x < 220 and 80 <= y < 160:
                        color = (20 if index % 2 == 0 else 230, 80, 150)
                    else:
                        color = [(20, 30, 230), (30, 220, 20), (220, 20, 30), (180, 180, 180)][(y >= 120) * 2 + (x >= 160)]
                    frame.extend((*((230, 230, 20) if replaced else color), 255))
            process.stdin.write(frame)
        process.stdin.close()
        if process.wait() != 0:
            raise SystemExit("Synthetic video encoding failed")
subprocess.run(["ffmpeg", "-v", "error", "-stream_loop", "-1", "-i", str(root / "moving.mp4"),
                "-t", "300", "-c", "copy", "-use_editlist", "0", "-movflags",
                "empty_moov+frag_keyframe+default_base_moof+skip_trailer", str(root / "long.mp4")], check=True)
for name in ["moving", "other", "long"]:
    metadata = json.loads(subprocess.check_output([
        "ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries",
        "stream=color_primaries,color_transfer,color_space", "-of", "json", str(root / f"{name}.mp4")]))
    if metadata["streams"] != [{"color_space": "smpte170m", "color_transfer": "iec61966-2-1", "color_primaries": "bt709"}]:
        raise SystemExit("Synthetic video lost its color description")
PY
cargo test --locked -p speakeasy-platform native_movie_preserves_exact_samples_pixels_and_input_ownership -- --ignored --nocapture --test-threads=1
