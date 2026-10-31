"""The publisher binds installer bytes to the embedded key and an exact release version."""

import hashlib
import json
import subprocess
from pathlib import Path
from typing import cast

import pytest

from scripts.sign_update import MAX_INSTALLER, NAME, openssl, sign_update


def fixture(tmp_path: Path) -> tuple[Path, Path, Path]:
    private = tmp_path / "fixture.pem"
    private.write_bytes(
        b"-----BEGIN PRIVATE KEY-----\n"
        + b"MC4CAQAwBQYDK2VwBCIEIAcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcH\n"
        + b"-----END PRIVATE KEY-----\n"
    )
    public = tmp_path / "fixture.pub"
    public.write_text("ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c")
    installer = tmp_path / NAME
    installer.write_bytes(b"synthetic installer")
    return installer, public, private


def test_signed_bytes_bind_installer_and_version(tmp_path: Path) -> None:
    installer, public, private = fixture(tmp_path)
    sign_update(installer, "1.2.3", public, private)
    manifest = tmp_path / "update.json"
    metadata = cast(object, json.loads(manifest.read_bytes()))
    assert metadata == {
        "schema": "speakeasy.update.v1",
        "version": "1.2.3",
        "target": "windows-x64",
        "file": NAME,
        "size": len(b"synthetic installer"),
        "sha256": hashlib.sha256(b"synthetic installer").hexdigest(),
    }
    verification_key = tmp_path / "verification.pem"
    verification_key.write_bytes(openssl(["pkey", "-in", str(private), "-pubout"]))
    command = [
        "openssl",
        "pkeyutl",
        "-verify",
        "-rawin",
        "-pubin",
        "-inkey",
        str(verification_key),
        "-in",
        str(manifest),
        "-sigfile",
        str(tmp_path / "update.json.sig"),
    ]
    assert subprocess.run(command, check=False, capture_output=True).returncode == 0
    manifest.write_bytes(manifest.read_bytes().replace(b"1.2.3", b"1.2.4"))
    assert subprocess.run(command, check=False, capture_output=True).returncode != 0
    with pytest.raises(ValueError, match="existing update metadata"):
        sign_update(installer, "1.2.4", public, private)


@pytest.mark.parametrize("fault", ["missing-key", "different-key", "version", "empty", "oversized"])
def test_invalid_publication_leaves_no_signed_assets(tmp_path: Path, fault: str) -> None:
    installer, public, private = fixture(tmp_path)
    version = "01.2.3" if fault == "version" else "1.2.3"
    if fault in {"missing-key", "different-key"}:
        public.write_text("" if fault == "missing-key" else "00" * 32)
    if fault in {"empty", "oversized"}:
        with installer.open("wb") as output:
            output.truncate(0 if fault == "empty" else MAX_INSTALLER + 1)
    with pytest.raises(ValueError):
        sign_update(installer, version, public, private)
    assert not (tmp_path / "update.json").exists()
    assert not (tmp_path / "update.json.sig").exists()
