"""Launcher boundaries run public fixtures without touching the desktop."""

import shutil
import subprocess
import sys
import time
from pathlib import Path

import pytest

from scripts.check_demo_linux import belongs_to_demo, stop


def test_only_owned_ancestry_is_accepted(tmp_path: Path) -> None:
    for pid, parent in ((21, 20), (20, 10), (31, 30), (30, 31), (41, 1)):
        root = tmp_path / str(pid)
        root.mkdir()
        (root / "stat").write_text(f"{pid} (public fixture (with spaces)) S {parent}\n")
    assert belongs_to_demo(21, 10, tmp_path)
    assert not belongs_to_demo(41, 10, tmp_path)
    assert not belongs_to_demo(99, 10, tmp_path)
    assert not belongs_to_demo(30, 10, tmp_path)


@pytest.mark.skipif(sys.platform != "linux", reason="Uses a private Linux process group")
def test_cleanup_terminates_a_launcher_descendant(tmp_path: Path) -> None:
    ready = tmp_path / "ready"
    child = tmp_path / "child"
    fixture = """\
import os, signal, sys, time
from pathlib import Path
if os.fork() == 0:
    Path(sys.argv[2]).write_text(str(os.getpid()))
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    Path(sys.argv[1]).touch()
time.sleep(60)
"""
    process = subprocess.Popen(
        [sys.executable, "-c", fixture, str(ready), str(child)], start_new_session=True
    )
    try:
        deadline = time.monotonic() + 5
        while not ready.exists():
            assert time.monotonic() < deadline, "Public launcher fixture did not start"
            time.sleep(0.01)
        descendant = int(child.read_text())
        stop(process)
        assert process.poll() is not None
        # SIGKILL can leave a zombie awaiting its reaper; a live descendant is a leak.
        status = Path(f"/proc/{descendant}/stat")
        deadline = time.monotonic() + 3
        while status.exists():
            try:
                if status.read_text().rsplit(")", 1)[1].split()[0] in {"Z", "X"}:
                    break
            except FileNotFoundError:
                break
            assert time.monotonic() < deadline, "Owned launcher descendant is still alive"
            time.sleep(0.01)
    finally:
        stop(process)


@pytest.mark.skipif(sys.platform != "linux", reason="Exercises the POSIX Linux package launcher")
def test_apprun_preserves_arguments_and_library_path(tmp_path: Path) -> None:
    root = tmp_path / "package with spaces"
    binary = root / "usr" / "bin" / "speakeasy"
    binary.parent.mkdir(parents=True)
    binary.write_text('#!/bin/sh\nprintf "%s\\n" "$LD_LIBRARY_PATH" "$@"\n')
    binary.chmod(0o755)
    shutil.copyfile(Path("packaging/linux/AppRun"), root / "AppRun")
    link = tmp_path / "symlink launcher"
    link.symlink_to(root / "AppRun")
    result = subprocess.run(
        ["sh", str(link), "--config", "a b.json", "--demo"],
        env={"PATH": "/usr/bin:/bin", "LD_LIBRARY_PATH": "/public/lib"},
        check=True,
        capture_output=True,
        text=True,
    )
    assert result.stdout.splitlines() == [
        f"{root}/usr/lib:/public/lib",
        "--config",
        "a b.json",
        "--demo",
    ]


@pytest.mark.parametrize(
    "script,arguments",
    [
        ("package-linux.sh", ["--native-tar", "extra"]),
        ("package-macos.sh", ["extra"]),
        ("check-rendering-linux.sh", ["--packages", "extra"]),
    ],
)
def test_extra_arguments_fail_before_build_or_docker(
    tmp_path: Path,
    script: str,
    arguments: list[str],
) -> None:
    marker = tmp_path / "unexpected-build"
    for tool in ("cargo", "docker"):
        stub = tmp_path / tool
        stub.write_text('#!/bin/sh\n: > "$BUILD_MARKER"\nexit 91\n')
        stub.chmod(0o755)
    result = subprocess.run(
        ["bash", f"scripts/{script}", *arguments],
        env={"PATH": f"{tmp_path}:/usr/bin:/bin", "BUILD_MARKER": str(marker)},
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode != 0
    assert "Usage:" in result.stderr
    assert not marker.exists()
