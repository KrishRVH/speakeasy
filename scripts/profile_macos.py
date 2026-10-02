#!/usr/bin/env python3
"""Sample explicit app and engine PIDs on macOS without reading command lines or user content.

Each interval reads cumulative CPU time, resident memory, and thread count through `ps`; the run
ends with each process's physical footprint from `vmmap -summary`, which counts compressed and
GPU-wired pages that resident memory misses. Energy, idle wakeups, and GPU time need Instruments
or `powermetrics`; this sampler reports only what unprivileged tools can see.
"""

import argparse
import json
import re
import statistics
import subprocess
import time
from collections.abc import Callable, Sequence
from pathlib import Path
from typing import TypedDict

type Run = Callable[[Sequence[str]], str]


class Sample(TypedDict):
    """Numeric process metadata; `started` distinguishes reuse of the same PID."""

    started: str
    elapsed_seconds: float
    cpu_seconds: float
    rss_bytes: int
    threads: int


class Summary(TypedDict):
    samples: int
    seconds: float
    average_cpu_one_core_percent: float | None
    median_rss_bytes: int | None
    max_rss_bytes: int | None
    max_threads: int | None
    footprint_bytes: int | None
    end: str | None


UNITS = {"": 1, "K": 1 << 10, "M": 1 << 20, "G": 1 << 30, "T": 1 << 40}
FOOTPRINT = re.compile(r"^Physical footprint:\s+([\d.]+)([KMGT]?)B?\s*$", re.M)


def cpu_seconds(text: str) -> float:
    """BSD `ps` CPU time: `[[days-]hours:]minutes:seconds.hundredths`."""
    days, _, clock = text.strip().rpartition("-")
    seconds = 0.0
    for part in clock.split(":"):
        seconds = seconds * 60 + float(part)
    return seconds + int(days or 0) * 86_400


def read_process(pid: int, run: Run) -> Sample:
    """One numeric snapshot of `pid`; raises `ProcessLookupError` once it is gone."""
    row = run(["ps", "-o", "lstart=,time=,rss=", "-p", str(pid)]).strip()
    if not row:
        raise ProcessLookupError("Process exited")
    *started, cpu, rss = row.split()
    # `ps -M` prints a header and one line per thread.
    threads = len(run(["ps", "-M", "-p", str(pid)]).strip().splitlines()) - 1
    if len(started) != 5 or threads < 1:
        raise ValueError("Incomplete process metadata")
    return {
        "started": " ".join(started),
        "elapsed_seconds": 0.0,
        "cpu_seconds": cpu_seconds(cpu),
        "rss_bytes": int(rss) * 1024,
        "threads": threads,
    }


def footprint(pid: int, run: Run) -> int | None:
    """Physical footprint in bytes, or `None` when `vmmap` cannot inspect the process."""
    try:
        match = FOOTPRINT.search(run(["vmmap", "-summary", str(pid)]))
    except subprocess.CalledProcessError:
        return None
    if match is None:
        return None
    return round(float(match[1]) * UNITS[match[2]])


def summarize(samples: Sequence[Sample], end: str | None, footprint: int | None) -> Summary:
    rss = sorted(sample["rss_bytes"] for sample in samples)
    seconds = samples[-1]["elapsed_seconds"] - samples[0]["elapsed_seconds"] if samples else 0.0
    cpu = samples[-1]["cpu_seconds"] - samples[0]["cpu_seconds"] if samples else 0.0
    return {
        "samples": len(samples),
        "seconds": seconds,
        "average_cpu_one_core_percent": 100 * cpu / seconds if seconds > 0 and cpu >= 0 else None,
        "median_rss_bytes": round(statistics.median(rss)) if rss else None,
        "max_rss_bytes": rss[-1] if rss else None,
        "max_threads": max((sample["threads"] for sample in samples), default=None),
        "footprint_bytes": footprint,
        "end": end,
    }


def record(
    pids: Sequence[int],
    seconds: float,
    interval: float,
    run: Run,
    now: Callable[[], float],
    sleep: Callable[[float], None],
) -> dict[int, Summary]:
    """Sample until `seconds` pass, stopping a PID at exit or reuse."""
    samples: dict[int, list[Sample]] = {pid: [] for pid in dict.fromkeys(pids)}
    ends: dict[int, str | None] = dict.fromkeys(samples)
    start = now()
    while True:
        elapsed = now() - start
        for pid, taken in samples.items():
            if ends[pid] is not None:
                continue
            try:
                sample = read_process(pid, run)
            except ProcessLookupError:
                ends[pid] = "exited"
                continue
            if taken and sample["started"] != taken[0]["started"]:
                ends[pid] = "identity changed"
                continue
            sample["elapsed_seconds"] = elapsed
            taken.append(sample)
        if elapsed >= seconds or all(end is not None for end in ends.values()):
            break
        sleep(min(interval, seconds - elapsed))
    return {
        pid: summarize(taken, ends[pid], None if ends[pid] else footprint(pid, run))
        for pid, taken in samples.items()
    }


def run_command(arguments: Sequence[str]) -> str:
    result = subprocess.run(arguments, capture_output=True, text=True, check=False)
    if result.returncode != 0 and arguments[0] == "vmmap":
        raise subprocess.CalledProcessError(result.returncode, arguments)
    return result.stdout


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pid", type=int, action="append", required=True)
    parser.add_argument("--label", required=True)
    parser.add_argument("--seconds", type=float, default=30.0)
    parser.add_argument("--interval", type=float, default=1.0)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    pids = [int(pid) for pid in arguments.pid]  # pyright: ignore[reportAny]
    seconds = float(arguments.seconds)  # pyright: ignore[reportAny]
    interval = float(arguments.interval)  # pyright: ignore[reportAny]
    output = Path(arguments.output)  # pyright: ignore[reportAny]
    label = str(arguments.label)  # pyright: ignore[reportAny]
    results = record(pids, seconds, interval, run_command, time.monotonic, time.sleep)
    output.parent.mkdir(parents=True, exist_ok=True)
    report = {"label": label, "processes": {str(pid): result for pid, result in results.items()}}
    output.write_text(json.dumps(report, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
