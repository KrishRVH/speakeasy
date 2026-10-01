"""Process sampling uses public /proc fixtures and an explicit clock."""

from pathlib import Path

import pytest

from scripts.profile_linux import Sample, read_process, record, summarize


@pytest.fixture
def proc(tmp_path: Path) -> Path:
    process = tmp_path / "42"
    process.mkdir()
    # Linux /proc/PID/stat: name, state, CPU ticks, thread count, start time, size.
    (process / "stat").write_text(
        "42 (public fixture (with spaces)) S 1 0 0 0 0 0 0 0 0 0 100 25 0 0 0 0 3 0 777 4096\n"
    )
    (process / "status").write_text("VmRSS:\t12 kB\nVmHWM:\t20 kB\n")
    return tmp_path


def test_numeric_snapshot_excludes_command_name_and_preserves_unknowns(proc: Path) -> None:
    sample = read_process(42, proc, 100)
    assert sample["identity"] == 777
    assert sample["cpu_seconds"] == 1.25
    assert sample["threads"] == 3
    assert sample["virtual_bytes"] == 4096
    assert sample["rss_bytes"] == 12 * 1024
    assert sample["pss_bytes"] is None
    assert sample["private_bytes"] is None
    assert sample["descriptors"] is None
    assert "public fixture" not in str(sample)


@pytest.mark.parametrize("stat", ["42 (public) Z", "42 (public) S 1", "malformed"])
def test_incomplete_metadata_cannot_become_a_measurement(proc: Path, stat: str) -> None:
    (proc / "42" / "stat").write_text(stat)
    with pytest.raises(ValueError, match="Incomplete process metadata"):
        read_process(42, proc, 100)


def test_optional_memory_is_parsed_in_bytes(proc: Path) -> None:
    process = proc / "42"
    (process / "fd").mkdir()
    (process / "fd" / "0").touch()
    (process / "smaps_rollup").write_text(
        "Pss: 9 kB\nPrivate_Clean: 3 kB\nPrivate_Dirty: 2 kB\nPrivate_Hugetlb: 1 kB\n"
    )
    sample = read_process(42, proc, 100)
    assert sample["descriptors"] == 1
    assert sample["pss_bytes"] == 9 * 1024
    assert sample["private_bytes"] == 6 * 1024


@pytest.mark.parametrize("state", ["Z", "X"])
def test_exited_process_is_not_reported_as_idle(proc: Path, state: str) -> None:
    stat = proc / "42" / "stat"
    stat.write_text(stat.read_text().replace(") S ", f") {state} "))
    with pytest.raises(ProcessLookupError, match="Process exited"):
        read_process(42, proc, 100)


class Clock:
    def __init__(self) -> None:
        self.elapsed: float = 0

    def now(self) -> float:
        return self.elapsed

    def sleep(self, duration: float) -> None:
        self.elapsed += duration


@pytest.mark.parametrize("fault", ["PID reused", "CPU counter regressed", "ProcessLookupError"])
def test_invalid_process_does_not_contaminate_previous_samples(proc: Path, fault: str) -> None:
    first = read_process(42, proc, 100)
    clock = Clock()

    def read(_: int) -> Sample:
        sample = first.copy()
        if clock.elapsed:
            if fault == "ProcessLookupError":
                raise ProcessLookupError("Public fixture exited")
            if fault == "PID reused":
                sample["identity"] += 1
            else:
                sample["cpu_seconds"] = 0
        return sample

    result = record([42], 10, 1, 4, read, clock.now, clock.sleep)[42]
    assert result["end"] == fault
    assert len(result["samples"]) == 1
    assert clock.elapsed == 1


def test_nearest_rank_p95_and_cpu_denominators(proc: Path) -> None:
    first = read_process(42, proc, 100)
    samples: list[Sample] = []
    for elapsed in range(20):
        sample = first.copy()
        sample["elapsed_seconds"] = elapsed
        sample["cpu_seconds"] = elapsed / 2
        sample["rss_bytes"] = elapsed + 1
        samples.append(sample)
    summary = summarize(samples, 4)
    assert summary["rss_bytes"] == {"median": 10.5, "p95": 19, "max": 20}
    assert "pss_bytes" not in summary
    assert summary["cpu_time_seconds"] == 9.5
    assert summary["average_cpu_one_core_percent"] == 50
    assert summary["average_cpu_machine_percent"] == 12.5


def test_identical_clock_readings_do_not_divide_by_zero(proc: Path) -> None:
    first = read_process(42, proc, 100)
    assert "average_cpu_one_core_percent" not in summarize([first, first.copy()], 4)


def test_deadline_and_duplicate_pids(proc: Path) -> None:
    clock = Clock()
    result = record(
        [42, 42], 2, 1, 4, lambda pid: read_process(pid, proc, 100), clock.now, clock.sleep
    )
    assert list(result) == [42]
    assert clock.elapsed == 2
    assert len(result[42]["samples"]) == 3
    assert result[42]["samples"][-1]["cpu_one_core_percent"] == 0
