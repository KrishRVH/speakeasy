"""Distribution fails closed around notarization; command stubs never sign or upload a binary."""

import json
import shutil
import subprocess
import sys
from pathlib import Path

import pytest


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
import json, os, sys, shutil
from pathlib import Path
name = Path(sys.argv[0]).name
with open("commands.jsonl", "a") as log:
    log.write(json.dumps([name, *sys.argv[1:]]) + "\\n")
if name == "uname":
    print("Darwin")
if name == "xcrun" and sys.argv[1] == "notarytool":
    rejected = sys.argv[3].endswith(os.environ.get("STUB_NOTARY_SUFFIX", ""))
    status = os.environ.get("STUB_NOTARY_STATUS", "Accepted") if rejected else "Accepted"
    print(json.dumps({"id": "synthetic-submission", "status": status}))
if name == "ditto":
    if "-c" in sys.argv:
        Path(sys.argv[-1]).write_text("synthetic archive")
    else:
        shutil.copytree(sys.argv[-2], sys.argv[-1])
if name == "hdiutil":
    Path(sys.argv[-1]).write_text("synthetic disk image")
if name == os.environ.get("STUB_FAIL_TOOL") and sys.argv[-1].endswith(os.environ.get("STUB_FAIL_SUFFIX", "")):
    sys.exit(1)
"""
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
    result, commands = package(tmp_path, environment)
    assert result.returncode == 0, result.stderr
    assert (tmp_path / "artifacts/speakeasy-macos.zip").exists()
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
    assert (tmp_path / "artifacts/speakeasy-macos.dmg").exists()
    assert not list((tmp_path / "artifacts").glob(".macos-package.*"))


@pytest.mark.parametrize(
    "failure",
    [
        {"STUB_NOTARY_STATUS": "Invalid"},
        {"STUB_FAIL_TOOL": "cargo"},
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
    artifacts.mkdir()
    for name in ["speakeasy-macos.zip", "speakeasy-macos.dmg"]:
        (artifacts / name).write_text("stale archive")
    result, _ = package(
        tmp_path,
        {
            "SPEAKEASY_MAC_SIGN_IDENTITY": "Synthetic Developer ID",
            "SPEAKEASY_MAC_NOTARY_PROFILE": "test-profile",
            **failure,
        },
    )
    assert result.returncode != 0
    assert not (tmp_path / "artifacts/speakeasy-macos.zip").exists()
    assert not (artifacts / "speakeasy-macos.dmg").exists()
    assert not list(artifacts.glob(".macos-package.*"))
    if "STUB_NOTARY_STATUS" in failure:
        assert "notarization" in result.stderr.lower()


def test_partial_distribution_configuration_fails_before_building(tmp_path: Path) -> None:
    result, commands = package(tmp_path, {"SPEAKEASY_MAC_SIGN_IDENTITY": "Synthetic Developer ID"})
    assert result.returncode != 0
    assert not any(command[0] == "cargo" for command in commands)
