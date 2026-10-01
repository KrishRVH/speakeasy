#!/usr/bin/env python3
"""Check that the pinned compiler enforces ownership and narrow lint exceptions."""

import json
import subprocess
import tempfile
from pathlib import Path
from typing import cast


def diagnostic_codes(stderr: str) -> set[str]:
    codes: set[str] = set()
    for line in stderr.splitlines():
        value = cast(object, json.loads(line))
        if isinstance(value, dict):
            code = cast(dict[str, object], value).get("code")
            if isinstance(code, dict):
                name = cast(dict[str, object], code).get("code")
                if isinstance(name, str):
                    codes.add(name)
    return codes


def main() -> None:
    scratch = Path(".scratch")
    scratch.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="rust-policy-", dir=scratch) as temporary:
        directory = Path(temporary)
        probes = {
            "lock": ("pub fn probe(_: std::sync::Mutex<u8>) {}", "clippy::disallowed_types"),
            "cell": ("pub fn probe(_: std::cell::RefCell<u8>) {}", "clippy::disallowed_types"),
            "atomic": (
                "pub fn probe(_: std::sync::atomic::AtomicU64) {}",
                "clippy::disallowed_types",
            ),
            "allow": (
                '#[allow(dead_code, reason = "probe")] pub fn probe() {}',
                "clippy::allow_attributes",
            ),
            "expect": (
                "#![deny(unfulfilled_lint_expectations)]\n"
                + '#[expect(clippy::disallowed_types, reason = "Native callback control cannot wait for owner messages")]\n'
                + "pub fn probe(_: std::sync::atomic::AtomicU64) {}",
                None,
            ),
        }
        for name, (body, expected) in probes.items():
            source = directory / f"{name}.rs"
            source.write_text(body + "\n")
            result = subprocess.run(
                [
                    "clippy-driver",
                    str(source),
                    "--crate-type=lib",
                    "--edition=2024",
                    "--emit=metadata",
                    "--out-dir",
                    str(directory),
                    "--error-format=json",
                    "-Dclippy::disallowed_types",
                    "-Fclippy::allow_attributes",
                    "-Fclippy::allow_attributes_without_reason",
                ],
                check=False,
                capture_output=True,
                text=True,
                timeout=30,
            )
            if expected is None:
                valid = result.returncode == 0
            else:
                valid = result.returncode != 0 and expected in diagnostic_codes(result.stderr)
            if not valid:
                raise SystemExit(
                    f"Rust policy probe {name!r} did not enforce its compiler contract"
                )
    print("Rust ownership, forbidden allow, and reasoned expect probes passed.")


if __name__ == "__main__":
    main()
