"""Process sampling parses public `ps` and `vmmap` fixtures under an explicit clock."""

import subprocess
from collections.abc import Sequence

import pytest

from scripts.profile_macos import cpu_seconds, footprint, read_process, record

STARTED = "Thu Oct  1 22:16:36 2026"


def ps(rows: dict[int, str], threads: int = 3):
    def run(arguments: Sequence[str]) -> str:
        if arguments[0] == "vmmap":
            return "Physical footprint:         12.5M\nPhysical footprint (peak):  20.0M\n"
        if arguments[1] == "-M":
            return "USER PID TT %CPU\n" + "user 42 ?? 0.0\n" * threads
        return rows.get(int(arguments[-1]), "")

    return run


@pytest.mark.parametrize(
    ("text", "seconds"),
    [("0:01.25", 1.25), ("61:02.50", 3662.5), ("1:00:00.00", 3600.0), ("2-01:00:00.00", 176400.0)],
)
def test_cpu_time_formats(text: str, seconds: float) -> None:
    assert cpu_seconds(text) == seconds


def test_snapshot_is_numeric_and_excludes_the_command() -> None:
    sample = read_process(42, ps({42: f"{STARTED} 0:01.25 2048\n"}))
    assert sample == {
        "started": " ".join(STARTED.split()),
        "elapsed_seconds": 0.0,
        "cpu_seconds": 1.25,
        "rss_bytes": 2048 * 1024,
        "threads": 3,
    }


def test_an_exited_process_is_not_reported_as_idle() -> None:
    with pytest.raises(ProcessLookupError):
        read_process(42, ps({}))


def test_incomplete_metadata_cannot_become_a_measurement() -> None:
    with pytest.raises(ValueError, match="Incomplete process metadata"):
        read_process(42, ps({42: "Thu 0:01.25 2048"}))


def test_footprint_is_reported_in_bytes_or_unknown() -> None:
    assert footprint(42, ps({})) == round(12.5 * (1 << 20))

    def refused(arguments: Sequence[str]) -> str:
        raise subprocess.CalledProcessError(1, arguments)

    assert footprint(42, refused) is None


def test_recording_stops_a_pid_that_is_reused() -> None:
    clock = iter([0.0, 0.0, 1.0, 2.0])
    rows = iter(
        [f"{STARTED} 0:01.00 1024", f"{STARTED} 0:01.50 3072", "Fri Oct  2 01:00:00 2026 0:00.01 1"]
    )
    current = {42: ""}

    def run(arguments: Sequence[str]) -> str:
        if arguments[0] == "ps" and arguments[1] != "-M":
            current[42] = next(rows)
        return ps(current)(arguments)

    summary = record([42], 2.0, 1.0, run, lambda: next(clock), lambda _: None)[42]
    assert summary["end"] == "identity changed"
    assert summary["samples"] == 2
    assert summary["average_cpu_one_core_percent"] == 50.0
    assert summary["max_rss_bytes"] == 3072 * 1024
    assert summary["footprint_bytes"] is None
