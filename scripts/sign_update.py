"""Sign the exact Windows update manifest with the key embedded in the application."""

import argparse
import hashlib
import json
import re
import subprocess
import tempfile
from pathlib import Path

NAME = "speakeasy-windows-x64-setup.exe"
MAX_INSTALLER = 512 * 1024 * 1024
ED25519_SPKI = bytes.fromhex("302a300506032b6570032100")


def openssl(arguments: list[str]) -> bytes:
    result = subprocess.run(["openssl", *arguments], capture_output=True, check=False)
    if result.returncode:
        raise ValueError(
            "OpenSSL could not authenticate the update signing key or sign the manifest."
        )
    return result.stdout


def sign_update(installer: Path, version: str, public_key: Path, private_key: Path) -> None:
    if not re.fullmatch(r"(?:0|[1-9][0-9]*)(?:\.(?:0|[1-9][0-9]*)){2}", version) or any(
        int(part) > 0xFFFFFFFF for part in version.split(".")
    ):
        raise ValueError("The update version must have three canonical unsigned 32-bit numbers.")
    embedded = public_key.read_text().strip()
    if not re.fullmatch(r"[0-9a-fA-F]{64}", embedded):
        raise ValueError("Configure the embedded Ed25519 public key before publishing updates.")
    actual = openssl(
        ["pkey", "-in", str(private_key), "-passin", "pass:", "-pubout", "-outform", "DER"]
    )
    if actual != ED25519_SPKI + bytes.fromhex(embedded):
        raise ValueError("The update signing key does not match the application's embedded key.")
    if installer.name != NAME or installer.is_symlink() or not installer.is_file():
        raise ValueError("Sign the regular packaged Windows installer under its release filename.")
    with installer.open("rb") as source:
        size = source.seek(0, 2)
        if not 1 <= size <= MAX_INSTALLER:
            raise ValueError("The installer is empty or exceeds the application's update limit.")
        source.seek(0)
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    manifest = {
        "schema": "speakeasy.update.v1",
        "version": version,
        "target": "windows-x64",
        "file": NAME,
        "size": size,
        "sha256": digest,
    }
    destination = installer.parent / "update.json"
    signature = installer.parent / "update.json.sig"
    if destination.exists() or signature.exists():
        raise ValueError("Remove existing update metadata before signing a new publication.")
    with tempfile.TemporaryDirectory(prefix=".update-", dir=installer.parent) as temporary:
        staged = Path(temporary) / "update.json"
        staged.write_bytes(json.dumps(manifest, separators=(",", ":")).encode() + b"\n")
        signed = openssl(
            [
                "pkeyutl",
                "-sign",
                "-rawin",
                "-inkey",
                str(private_key),
                "-passin",
                "pass:",
                "-in",
                str(staged),
            ]
        )
        if len(signed) != 64:
            raise ValueError("The update signature is not Ed25519.")
        staged_signature = Path(temporary) / "update.json.sig"
        staged_signature.write_bytes(signed)
        staged_signature.replace(signature)
        staged.replace(destination)


class Arguments(argparse.Namespace):
    def __init__(self) -> None:
        super().__init__()
        self.installer: Path = Path(".")
        self.version: str = ""
        self.public_key: Path = Path("packaging/update-key.pub")
        self.private_key: Path = Path(".")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("installer", type=Path)
    parser.add_argument("version")
    parser.add_argument("--public-key", type=Path, default=Path("packaging/update-key.pub"))
    parser.add_argument("--private-key", type=Path, required=True)
    arguments = parser.parse_args(namespace=Arguments())
    try:
        sign_update(
            arguments.installer, arguments.version, arguments.public_key, arguments.private_key
        )
    except (OSError, ValueError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
