"""Distribution fails closed around notarization; command stubs never sign or upload a binary."""

import json
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.skipif(
    sys.platform == "win32", reason="Runs the Mac packaging script through POSIX command stubs"
)
OUTPUTS = ("Speakeasy.app", "speakeasy-macos.zip", "speakeasy-macos.dmg")


def seed_stale_outputs(artifacts: Path) -> None:
    (artifacts / "Speakeasy.app/Contents").mkdir(parents=True)
    (artifacts / "Speakeasy.app/Contents/stale").write_text("stale app")
    for name in ["speakeasy-macos.zip", "speakeasy-macos.dmg"]:
        (artifacts / name).write_text("stale archive")


def package(
    tmp_path: Path, signing: dict[str, str]
) -> tuple[subprocess.CompletedProcess[str], list[list[str]]]:
    for name in (
        "scripts/package-macos.sh",
        "Cargo.toml",
        "settings.example.json",
        "LICENSE",
        "packaging/macos/Info.plist",
        "packaging/macos/Entitlements.plist",
        "packaging/macos/Speakeasy.icns",
        "crates/app/assets/JosefinSans-OFL.txt",
        "crates/show/NOTICE.md",
    ):
        target = tmp_path / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(name, target)
    binary = tmp_path / "target/release/speakeasy"
    binary.parent.mkdir(parents=True)
    binary.write_text("synthetic executable")
    tools = tmp_path / "tools"
    tools.mkdir()
    script = """#!/usr/bin/env python3
import fcntl, json, os, sys, shutil
from pathlib import Path
name = Path(sys.argv[0]).name
if name != "uname":
    with open("artifacts/.package-macos.lock", "a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            pass
        else:
            Path("unlocked").write_text(name)
with open("commands.jsonl", "a") as log:
    log.write(json.dumps([name, *sys.argv[1:]]) + "\\n")
with open("published.jsonl", "a") as log:
    visible = [output for output in OUTPUTS if Path("artifacts", output).exists()]
    log.write(json.dumps([name, *visible]) + "\\n")
if name == "uname":
    print("Darwin")
if name == "xcrun" and sys.argv[1] == "notarytool":
    targeted = sys.argv[3].endswith(os.environ.get("STUB_NOTARY_SUFFIX", ""))
    status = os.environ.get("STUB_NOTARY_STATUS", "Accepted") if targeted else "Accepted"
    print(json.dumps({"id": "synthetic-submission", "status": status}))
if name == "ditto":
    if "-c" in sys.argv:
        Path(sys.argv[-1]).write_text("synthetic archive")
    else:
        shutil.copytree(sys.argv[-2], sys.argv[-1])
if name == "hdiutil":
    Path(sys.argv[-1]).write_text("synthetic disk image")
    if os.environ.get("STUB_OCCUPY_APP"):
        Path("artifacts/Speakeasy.app/Contents").mkdir(parents=True)
        Path("artifacts/Speakeasy.app/Contents/occupied").write_text("another build")
    if os.environ.get("STUB_OCCUPY_DMG"):
        Path("artifacts/speakeasy-macos.dmg").mkdir()
if name == os.environ.get("STUB_FAIL_TOOL") and sys.argv[-1].endswith(os.environ.get("STUB_FAIL_SUFFIX", "")):
    sys.exit(1)
""".replace("OUTPUTS", repr(OUTPUTS))
    for name in ("uname", "cargo", "plutil", "codesign", "ditto", "xcrun", "spctl", "hdiutil"):
        stub = tools / name
        stub.write_text(script)
        stub.chmod(0o755)
    environment = {"PATH": f"{tools}:{Path(sys.executable).parent}:/usr/bin:/bin", **signing}
    result = subprocess.run(
        ["bash", "scripts/package-macos.sh"],
        cwd=tmp_path,
        env=environment,
        check=False,
        capture_output=True,
        text=True,
    )
    commands = [json.loads(line) for line in (tmp_path / "commands.jsonl").read_text().splitlines()]
    return result, commands


@pytest.mark.parametrize("signed", [False, True])
def test_archive_appears_only_after_signature_and_distribution_acceptance(
    tmp_path: Path, *, signed: bool
) -> None:
    environment = (
        {
            "SPEAKEASY_MAC_SIGN_IDENTITY": "Synthetic Developer ID",
            "SPEAKEASY_MAC_NOTARY_PROFILE": "test-profile",
        }
        if signed
        else {}
    )
    artifacts = tmp_path / "artifacts"
    seed_stale_outputs(artifacts)
    result, commands = package(tmp_path, environment)
    assert result.returncode == 0, result.stderr
    log = (tmp_path / "published.jsonl").read_text().splitlines()
    published: list[list[str]] = [json.loads(line) for line in log]
    early = [entry for entry in published if entry[0] != "uname" and entry[1:]]
    assert not early, f"Outputs were visible while tools still ran: {early}"
    assert not (tmp_path / "unlocked").exists(), "A build tool ran without the package lock"
    assert (artifacts / "Speakeasy.app/Contents/MacOS/speakeasy").is_file()
    assert not (artifacts / "Speakeasy.app/Contents/stale").exists()
    assert (artifacts / "speakeasy-macos.zip").read_text() == "synthetic archive"
    signing = next(command for command in commands if command[:2] == ["codesign", "--force"])
    assert ("runtime" in signing) == signed
    notarization = [command for command in commands if command[0] == "xcrun"]
    if signed:
        assert [command[1:3] for command in notarization] == [
            ["notarytool", "submit"],
            ["stapler", "staple"],
            ["stapler", "validate"],
        ] * 2
        assert commands[-1][0] == "spctl"
    else:
        assert not notarization
    assert (artifacts / "speakeasy-macos.dmg").read_text() == "synthetic disk image"
    assert not list(artifacts.glob(".macos-package.*"))


@pytest.mark.parametrize(
    "failure",
    [
        {"STUB_NOTARY_STATUS": "Invalid"},
        {"STUB_FAIL_TOOL": "cargo"},
        {"STUB_FAIL_TOOL": "codesign"},
        {"STUB_FAIL_TOOL": "xcrun"},
        {"STUB_FAIL_TOOL": "spctl"},
        {"STUB_FAIL_TOOL": "hdiutil"},
        {"STUB_NOTARY_STATUS": "Invalid", "STUB_NOTARY_SUFFIX": ".dmg"},
        {"STUB_FAIL_TOOL": "xcrun", "STUB_FAIL_SUFFIX": ".dmg"},
        {"STUB_FAIL_TOOL": "spctl", "STUB_FAIL_SUFFIX": ".dmg"},
    ],
)
def test_failed_distribution_never_emits_a_release_archive(
    tmp_path: Path, failure: dict[str, str]
) -> None:
    artifacts = tmp_path / "artifacts"
    seed_stale_outputs(artifacts)
    result, _ = package(
        tmp_path,
        {
            "SPEAKEASY_MAC_SIGN_IDENTITY": "Synthetic Developer ID",
            "SPEAKEASY_MAC_NOTARY_PROFILE": "test-profile",
            **failure,
        },
    )
    assert result.returncode != 0
    assert not (artifacts / "Speakeasy.app").exists()
    assert not (artifacts / "speakeasy-macos.zip").exists()
    assert not (artifacts / "speakeasy-macos.dmg").exists()
    assert not list(artifacts.glob(".macos-package.*"))
    if "STUB_NOTARY_STATUS" in failure:
        rejected = "disk image" if failure.get("STUB_NOTARY_SUFFIX") == ".dmg" else "app"
        assert f"notarization of the {rejected}. Check submission synthetic-submission" in (
            result.stderr
        )


def test_partial_distribution_configuration_fails_before_building(tmp_path: Path) -> None:
    artifacts = tmp_path / "artifacts"
    seed_stale_outputs(artifacts)
    result, commands = package(tmp_path, {"SPEAKEASY_MAC_SIGN_IDENTITY": "Synthetic Developer ID"})
    assert result.returncode != 0
    assert not any(command[0] == "cargo" for command in commands)
    assert not any((artifacts / name).exists() for name in OUTPUTS)


def test_a_second_run_leaves_the_running_package_alone(tmp_path: Path) -> None:
    import fcntl

    artifacts = tmp_path / "artifacts"
    seed_stale_outputs(artifacts)
    with (artifacts / ".package-macos.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        result, commands = package(tmp_path, {})
    assert result.returncode != 0
    assert "Another macOS package run is using artifacts" in result.stderr
    assert not any(command[0] == "cargo" for command in commands)
    assert (artifacts / "Speakeasy.app/Contents/stale").exists()
    assert all((artifacts / name).read_text() == "stale archive" for name in OUTPUTS[1:])


def test_a_failed_disk_image_rename_takes_back_the_app_and_archive(tmp_path: Path) -> None:
    artifacts = tmp_path / "artifacts"
    result, _ = package(tmp_path, {"STUB_OCCUPY_DMG": "1"})
    assert result.returncode != 0
    assert not (artifacts / "Speakeasy.app").exists()
    assert not (artifacts / "speakeasy-macos.zip").exists()
    assert (artifacts / "speakeasy-macos.dmg").is_dir()


def test_archives_stay_unpublished_when_the_app_cannot_be_replaced(tmp_path: Path) -> None:
    artifacts = tmp_path / "artifacts"
    result, _ = package(tmp_path, {"STUB_OCCUPY_APP": "1"})
    assert result.returncode != 0
    assert (artifacts / "Speakeasy.app/Contents/occupied").exists()
    assert not (artifacts / "speakeasy-macos.zip").exists()
    assert not (artifacts / "speakeasy-macos.dmg").exists()
