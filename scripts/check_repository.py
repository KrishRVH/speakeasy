#!/usr/bin/env python3
"""Check the tree as it would be committed: tracked and new, non-ignored files outside vendor/.

Findings: history directories at the top level or under docs/; old, legacy, deprecated, bak,
orig, or vN file names; handoff notes; dated top-level or docs names; `mise run` references to
tasks that do not exist; and Cargo workspace members that skip the workspace lint policy.
"""

import json
import os
import re
import shutil
import subprocess
import sys
import tomllib
from collections.abc import Mapping
from pathlib import Path, PurePosixPath
from typing import NotRequired, TypedDict, cast

# Externally owned trees that every rule skips.
ALLOWED_PATHS = ("vendor/",)
# Directories that hold history rather than current contracts, checked at the
# top level and under docs/, where sediment collects.
HISTORY_DIRECTORIES = {
    "adr",
    "archive",
    "audits",
    "briefs",
    "deprecated",
    "evidence",
    "legacy",
    "old",
    "reports",
    "releases",
    "research",
}
LEFTOVER_TOKEN = re.compile(r"^(old|legacy|deprecated|bak|orig)$")
VERSION_TOKEN = re.compile(r"^v\d+$")
# A note file whose stem holds only these tokens is a handoff note.
NOTE_TOKENS = {"handoff", "pickup", "notes"}
NOTE_SUFFIXES = {".md", ".mdx", ".txt"}
DATED_NAME = re.compile(r"(19|20)\d\d-\d\d-\d\d")
MISE_RUN = re.compile(r"\bmise run ([A-Za-z0-9][\w:.*-]*)")
CODE_SPAN = re.compile(rb"```[\s\S]*?```|`[^`\n]+`")
REFERENCE_SUFFIXES = {".md", ".mdx", ".toml", ".yml", ".yaml", ".sh", ".bash"}


class Task(TypedDict):
    name: str
    aliases: NotRequired[list[str] | None]


class Package(TypedDict):
    manifest_path: str


class Metadata(TypedDict):
    workspace_root: str
    packages: list[Package]


class Lints(TypedDict, total=False):
    workspace: bool


class Manifest(TypedDict, total=False):
    lints: Lints


def main(environment: Mapping[str, str]) -> int:
    paths = committed_paths(environment)
    known = known_tasks(environment)
    findings = name_findings(paths) + workspace_lint_findings(environment)
    for path in paths:
        if PurePosixPath(path).suffix in REFERENCE_SUFFIXES:
            findings += task_reference_findings(path, known)
    for finding in findings:
        print(f"hygiene: {finding}", file=sys.stderr)
    return 1 if findings else 0


def run(*command: str, environment: Mapping[str, str]) -> bytes:
    tool = shutil.which(command[0])
    if tool is None:
        sys.exit(f"hygiene: `{command[0]}` is not on PATH")
    result = subprocess.run([tool, *command[1:]], check=False, capture_output=True, env=environment)
    if result.returncode != 0:
        sys.exit(f"hygiene: `{' '.join(command)}` failed: {os.fsdecode(result.stderr).strip()}")
    return result.stdout


def committed_paths(environment: Mapping[str, str]) -> list[str]:
    """Files the commit would hold that exist in the work tree, outside allowed paths."""
    listing = run(
        "git",
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
        "--deduplicate",
        environment=environment,
    )
    paths = (os.fsdecode(raw) for raw in listing.split(b"\0") if raw)
    return sorted(
        path for path in paths if os.path.isfile(path) and not path.startswith(ALLOWED_PATHS)
    )


def known_tasks(environment: Mapping[str, str]) -> set[str]:
    """Task names and aliases that `mise run` accepts here."""
    # Pinned mise owns this JSON schema.
    listing = cast(
        list[Task],
        json.loads(
            run("mise", "tasks", "ls", "--local", "--hidden", "--json", environment=environment)
        ),
    )
    return {name for task in listing for name in [task["name"], *(task.get("aliases") or [])]}


def tokens(name: str) -> list[str]:
    snake = re.sub(r"(?<=[a-z0-9])(?=[A-Z])", "_", name).lower()
    return [token for token in re.split(r"[_\-. ]+", snake) if token]


def name_findings(paths: list[str]) -> list[str]:
    findings: list[str] = []
    for path in paths:
        parts = PurePosixPath(path).parts
        top = parts[0]
        name = parts[-1]
        directories = parts[:-1]
        scoped = directories if top == "docs" else directories[:1]
        history = next((part for part in scoped if part.lower() in HISTORY_DIRECTORIES), None)
        name_tokens = tokens(name)
        stem_tokens = tokens(name.split(".")[0])
        if history:
            findings.append(f"{path}: history directory `{history}/`; git holds history")
        elif any(LEFTOVER_TOKEN.match(token) for token in name_tokens):
            findings.append(f"{path}: leftover file name; rename in place or delete it")
        elif any(VERSION_TOKEN.match(token) for token in name_tokens):
            findings.append(f"{path}: versioned file name; replace the old version in place")
        elif (
            PurePosixPath(name).suffix in NOTE_SUFFIXES
            and stem_tokens
            and set(stem_tokens) <= NOTE_TOKENS
        ):
            findings.append(f"{path}: handoff note; keep task notes in untracked .scratch/")
        elif len(parts) <= 2 and top in ("docs", name) and DATED_NAME.search(name):
            findings.append(f"{path}: dated name; current docs carry no dates")
    return findings


def task_reference_findings(path: str, known: set[str]) -> list[str]:
    data = Path(path).read_bytes()
    if path.endswith((".md", ".mdx")):
        data = b"\n".join(CODE_SPAN.findall(data))
    findings: set[str] = set()
    for match in MISE_RUN.finditer(data.decode("utf-8", errors="replace")):
        name = match.group(1).rstrip(".")
        # Wildcard selectors and `namespace:` placeholders name no single task.
        if "*" not in name and not name.endswith(":") and name not in known:
            findings.add(f"{path}: unknown mise task `{name}`")
    return sorted(findings)


def toml_file(path: Path) -> object:
    """Parsed TOML as `object`, which callers may cast to a TypedDict, unlike `dict[str, Any]`."""
    return tomllib.loads(path.read_text(encoding="utf-8"))


def workspace_lint_findings(environment: Mapping[str, str]) -> list[str]:
    """Cargo lets a workspace member skip the workspace lint policy; this repository does not."""
    # Cargo owns membership, including path dependencies that join the workspace without a
    # `members` entry. With --no-deps it resolves nothing and lists exactly the members.
    metadata = cast(
        Metadata,
        json.loads(
            run(
                "cargo",
                "metadata",
                "--no-deps",
                "--offline",
                "--format-version=1",
                environment=environment,
            )
        ),
    )
    findings: list[str] = []
    for package in metadata["packages"]:
        manifest = Path(package["manifest_path"])
        lints = cast(Manifest, toml_file(manifest)).get("lints")
        if lints is None or lints.get("workspace") is not True:
            shown = os.path.relpath(manifest, metadata["workspace_root"])
            findings.append(f"{shown}: workspace members must declare [lints] workspace = true")
    return findings


if __name__ == "__main__":
    # Snapshot config once at the composition root, including explicit Git overrides.
    sys.exit(main(dict(os.environ)))  # noqa: TID251
