#!/usr/bin/env python3
"""Check the tree as it would be committed: tracked and new, non-ignored files.

Paths are relative to the project root. Vendored files are excluded; maintained
paths are checked for leftover names and references to nonexistent mise tasks.
"""

import json
import os
import re
import shutil
import subprocess
import sys
from collections.abc import Mapping
from pathlib import Path, PurePosixPath
from typing import NotRequired, TypedDict, cast

# Externally owned or generated trees that every rule and the inventory skip,
# such as vendored code or a named third-party plugin directory.
ALLOWED_PATHS = ("vendor/",)
# Paths whose names follow an external contract, such as migrations named by
# version or date. Version and date rules skip them; leftover rules still apply.
# Examples: "api/", "proto/", "docs/blog/".
CONTRACT_PATHS: tuple[str, ...] = ()
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
# A literal `mise run` invocation in command position (line or code-span start,
# after a shell separator, a quote, `=`, or `:`) whose whole argument names a
# task, `//project:task`, `//:task`, or `:task`. An unqualified name only needs
# to exist somewhere in a monorepo; a qualified name must match exactly. Prose,
# selectors with `...` or `*`, placeholders ending in `:`, arguments joined to
# an expansion such as `build-"${TARGET}"`, and lines that change directory (`mise -C`,
# `mise --cd`, a `cd` or `pushd` command) are skipped.
MISE_RUN = re.compile(
    r"(?:^|[;&|(`\"'=:])\s*mise run ((?://[\w./-]*)?:?[A-Za-z0-9][\w:.*-]*)(\.\.\.)?(?=$|[\s;&|)]|[`\"'](?:$|[\s,.;:)\]}]))"
)
EXPLICIT_DIRECTORY = re.compile(r"\bmise (-C|--cd)\b|(?:^|[;&|(`\"']\s*|\s)(cd|pushd)\s")
DISCOVERY_FAILURE = re.compile(r"not a git repository \(or any")
CODE_SPAN = re.compile(rb"```[\s\S]*?```|`[^`\n]+`")
REFERENCE_SUFFIXES = {".md", ".mdx", ".toml", ".yml", ".yaml", ".sh", ".bash"}
TASK_DIRECTORY = ".config/mise/tasks/"
LOCKFILE = re.compile(
    r"(^|/)(Cargo\.lock|go\.sum|package-lock\.json|bun\.lockb?|yarn\.lock|pnpm-lock\.yaml|"
    + r"uv\.lock|poetry\.lock|composer\.lock|mix\.lock|mise\.lock|pylock[\w.-]*\.toml)$|\.(uid|import)$"
)
CODE = {".rs", ".py", ".sh", ".ps1"}
DOCS = {".md"}
TOOLING_DIRECTORIES = (".config/", ".github/", "scripts/", "tools/", "ci/")
TEST_DIRECTORIES = {"test", "tests", "spec", "specs", "__tests__", "e2e", "testdata", "fixtures"}
TEST_STEM = re.compile(r"^(tests?|conftest)$|^test_|(_test|_spec|\.test|\.spec|Tests?)$")
CHUNK = 1 << 16
MAX_REFERENCE_FILE = 4 << 20


def main(environment: Mapping[str, str]) -> int:
    environment = {**environment, "LC_ALL": "C", "LANGUAGE": ""}
    if not inside_work_tree(environment):
        print("hygiene: not a git work tree; skipped")
        return 0
    entries = committed_entries(environment)
    known = known_tasks(environment)
    findings = name_findings(entries)
    print_inventory(entries)
    for path, regular in entries.items():
        if regular and is_reference_file(path):
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


def inside_work_tree(environment: Mapping[str, str]) -> bool:
    git = shutil.which("git")
    if git is None:
        sys.exit("hygiene: `git` is not on PATH")
    result = subprocess.run(
        [git, "rev-parse", "--is-inside-work-tree"],
        check=False,
        capture_output=True,
        env=environment,
    )
    if result.returncode == 0:
        return result.stdout.strip() == b"true"
    error = os.fsdecode(result.stderr).strip()
    # Only ordinary discovery failure means "no repository"; broken explicit
    # metadata, such as an invalid GIT_DIR or gitfile, is an error.
    if DISCOVERY_FAILURE.search(error) and "GIT_DIR" not in environment:
        return False
    sys.exit(f"hygiene: git cannot read this work tree: {error}")


def committed_entries(environment: Mapping[str, str]) -> dict[str, bool]:
    """Map each path the commit would hold to whether its content can be read here."""
    entries: dict[str, bool] = {}
    for record in run("git", "ls-files", "-z", "-t", "--stage", environment=environment).split(
        b"\0"
    ):
        if not record:
            continue
        meta, raw_path = record.split(b"\t", 1)
        tag, mode = meta.split(b" ")[:2]
        path = os.fsdecode(raw_path)
        if mode == b"160000" or path in entries:
            continue
        if tag == b"S":
            entries[path] = False
        elif os.path.lexists(path) and not through_symlink(path):
            # With core.symlinks=false, git writes a symlink as a plain file.
            entries[path] = mode != b"120000" and regular_file(path)
    for raw_path in run(
        "git", "ls-files", "-z", "--others", "--exclude-standard", environment=environment
    ).split(b"\0"):
        path = os.fsdecode(raw_path)
        if path and not through_symlink(path):
            entries[path] = regular_file(path)
    return {
        path: regular
        for path, regular in sorted(entries.items())
        if not path.startswith(ALLOWED_PATHS)
    }


def through_symlink(path: str) -> bool:
    """Whether reaching `path` passes through a symlinked directory."""
    parts = PurePosixPath(path).parts[:-1]
    return any(os.path.islink(os.path.join(*parts[: index + 1])) for index in range(len(parts)))


def regular_file(path: str) -> bool:
    return os.path.isfile(path) and not os.path.islink(path)


class Task(TypedDict):
    name: str
    aliases: NotRequired[list[str] | None]


def known_tasks(environment: Mapping[str, str]) -> tuple[set[str], set[str]]:
    """Unqualified task names and aliases, and their `//project:` qualified forms."""
    # Pinned mise owns this JSON schema; local task references are checked below.
    listing = cast(
        list[Task],
        json.loads(
            run(
                "mise",
                "tasks",
                "ls",
                "--all",
                "--local",
                "--hidden",
                "--json",
                environment=environment,
            )
        ),
    )
    unqualified: set[str] = set()
    qualified: set[str] = set()
    for task in listing:
        project, name = split_task(task["name"])
        for local in [name, *(task.get("aliases") or [])]:
            unqualified.add(local)
            if project is not None:
                qualified.add(f"//{project}:{local}")
    return unqualified, qualified


def split_task(name: str) -> tuple[str | None, str]:
    """A monorepo task's project path and local name; no project outside a monorepo."""
    match = re.match(r"^//([^:]*):(.+)$", name)
    return (match.group(1), match.group(2)) if match else (None, name)


def tokens(name: str) -> list[str]:
    snake = re.sub(r"(?<=[a-z0-9])(?=[A-Z])", "_", name).lower()
    return [token for token in re.split(r"[_\-. ]+", snake) if token]


def name_findings(entries: dict[str, bool]) -> list[str]:
    findings: list[str] = []
    for path in entries:
        parts = PurePosixPath(path).parts
        if not parts:
            continue
        top = parts[0]
        name = parts[-1]
        directories = parts[:-1]
        scoped = directories if top == "docs" else directories[:1]
        history = next((part for part in scoped if part.lower() in HISTORY_DIRECTORIES), None)
        name_tokens = tokens(name)
        stem_tokens = tokens(name.split(".")[0])
        contract = path.startswith(CONTRACT_PATHS)
        if history:
            findings.append(f"{path}: history directory `{history}/`; git holds history")
        elif any(LEFTOVER_TOKEN.match(token) for token in name_tokens):
            findings.append(f"{path}: leftover file name; rename in place or delete it")
        elif not contract and any(VERSION_TOKEN.match(token) for token in name_tokens):
            findings.append(f"{path}: versioned file name; replace the old version in place")
        elif (
            PurePosixPath(name).suffix in NOTE_SUFFIXES
            and stem_tokens
            and set(stem_tokens) <= NOTE_TOKENS
        ):
            findings.append(f"{path}: handoff note; keep task notes in untracked .scratch/")
        elif not contract and len(parts) <= 2 and top in ("docs", name) and DATED_NAME.search(name):
            findings.append(f"{path}: dated name; current docs carry no dates")
    return findings


def is_test(path: str) -> bool:
    pure = PurePosixPath(path)
    return bool(TEST_DIRECTORIES & set(pure.parts[:-1])) or bool(TEST_STEM.search(pure.stem))


def category(path: str) -> str:
    suffix = PurePosixPath(path).suffix
    if LOCKFILE.search(path):
        return "generated"
    if suffix in DOCS:
        return "docs"
    if path.startswith(TOOLING_DIRECTORIES):
        return "tooling"
    if suffix in CODE:
        return "tests" if is_test(path) else "source"
    return "tooling" if "/" not in path else "other"


def text_lines(path: str) -> int | None:
    """Count lines in a text file by streaming it; None for a binary file."""
    lines = 0
    with open(path, "rb") as file:
        while chunk := file.read(CHUNK):
            if b"\0" in chunk:
                return None
            lines += chunk.count(b"\n")
    return lines


def print_inventory(entries: dict[str, bool]) -> None:
    counted = {
        name: [0, 0] for name in ("source", "tests", "docs", "tooling", "other", "generated")
    }
    largest: list[tuple[int, str]] = []
    for path, regular in entries.items():
        lines = text_lines(path) if regular else None
        if lines is None:
            continue
        kind = category(path)
        counted[kind][0] += 1
        counted[kind][1] += lines
        if kind == "source":
            largest = sorted([*largest, (lines, path)], reverse=True)[:5]
    print(f"{'category':<10} {'files':>6} {'lines':>8}")
    for name, (files, lines) in counted.items():
        print(f"{name:<10} {files:>6} {lines:>8}")
    if largest:
        print("largest source files:")
        for lines, path in largest:
            print(f"{lines:>8} {path}")


def is_reference_file(path: str) -> bool:
    reference = PurePosixPath(path).suffix in REFERENCE_SUFFIXES or path.startswith(TASK_DIRECTORY)
    return reference and not is_test(path)


def task_reference_findings(path: str, known: tuple[set[str], set[str]]) -> list[str]:
    unqualified, qualified = known
    if os.path.getsize(path) > MAX_REFERENCE_FILE:
        print(f"hygiene: {path}: over {MAX_REFERENCE_FILE >> 20} MiB; task references not checked")
        return []
    data = Path(path).read_bytes()
    if b"\0" in data:
        return []
    if path.endswith((".md", ".mdx")):
        data = b"\n".join(CODE_SPAN.findall(data))
    findings: set[str] = set()
    for line in data.decode("utf-8", errors="replace").splitlines():
        if EXPLICIT_DIRECTORY.search(line):
            continue
        for match in MISE_RUN.finditer(line):
            name = match.group(1).removeprefix(":").rstrip(".")
            if match.group(2) or name.endswith(":") or "*" in name or "..." in name:
                continue
            # Outside a monorepo, mise lists no qualified names; `//:task` is the root task.
            if name.startswith("//") and qualified:
                known_name = name in qualified
            else:
                known_name = re.sub(r"^//:", "", name) in unqualified
            if not known_name:
                findings.add(f"{path}: unknown mise task `{name}`")
    return sorted(findings)


if __name__ == "__main__":
    # Snapshot config once at the composition root, including explicit Git overrides.
    sys.exit(main(dict(os.environ)))  # noqa: TID251
