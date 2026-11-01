#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ $(uname -s) != Darwin || $# -ne 0 ]]; then
  echo "Run this owned-window update check on macOS without arguments." >&2
  exit 1
fi
mkdir -p artifacts
fixtures=$(mktemp -d artifacts/macos-update.XXXXXX)
cargo build --locked -p speakeasy --features native-acceptance --message-format=json-render-diagnostics > "$fixtures/build.json"
python3 - "$fixtures" << 'PY'
import json
import pathlib
import plistlib
import shutil
import subprocess
import sys
import time

root = pathlib.Path(sys.argv[1]).resolve()
records = [json.loads(line) for line in (root / "build.json").read_text().splitlines()]
binary = next(pathlib.Path(record["executable"]) for record in records
              if record.get("executable") and record.get("target", {}).get("name") == "speakeasy")
bundle = root / "Owned Quit.app"
contents = bundle / "Contents"
(contents / "MacOS").mkdir(parents=True)
executable = contents / "MacOS/speakeasy"
shutil.copy2(binary, executable)
(contents / "Info.plist").write_bytes(plistlib.dumps(dict(
    CFBundleIdentifier="dev.speakeasy.acceptance.quit", CFBundleName="Owned Quit",
    CFBundleExecutable="speakeasy", CFBundlePackageType="APPL", CFBundleVersion="1",
    CFBundleShortVersionString="0.0.0", LSUIElement=True, LSMinimumSystemVersion="14.0")))
subprocess.run(["codesign", "--force", "--sign", "-", str(bundle)], check=True)
for case in ("external", "borrowed", "foreground", "repeated", "approved"):
    log = root / f"{case}.log"
    children = []
    try:
        with log.open("w") as output:
            app = subprocess.Popen([str(executable), "--check-owned-quit", case], stdout=output, stderr=output)
            children.append(app)
            deadline = time.monotonic() + 30
            sent = case not in ("external", "repeated")
            while app.poll() is None and time.monotonic() < deadline:
                if not sent and "Owned Mac quit ready" in log.read_text():
                    sender = subprocess.Popen([str(executable), "--send-owned-quit", str(app.pid)], stdout=output, stderr=output)
                    children.append(sender)
                    sent = True
                time.sleep(0.02)
            for child in children:
                child.wait(timeout=max(0.1, deadline - time.monotonic()))
            text = log.read_text()
            if any(child.returncode != 0 for child in children) or "Owned Mac quit passed" not in text or "Owned Mac quit failed" in text:
                raise RuntimeError(f"{case}: native cleanup check failed\n{text[-16384:]}")
        print(f"{case}: accepted save, render callback, and instance release passed", flush=True)
    finally:
        for child in children:
            if child.poll() is None:
                child.kill()
            child.wait()
PY
