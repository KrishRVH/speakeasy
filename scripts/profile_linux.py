#!/usr/bin/env python3
"""Sample explicit app/worker PIDs without reading command lines or user content."""

import argparse
import json
import math
import os
import platform
import statistics
import time
from collections.abc import Callable, Sequence
from pathlib import Path
from typing import Literal, TypedDict


class Sample(TypedDict):
    """Numeric process metadata; identity distinguishes reuse of the same PID."""

    identity: int
    cpu_seconds: float
    elapsed_seconds: float
    rss_bytes: int
    peak_rss_bytes: int
    virtual_bytes: int
    threads: int
    main_thread_voluntary_switches: int | None
    main_thread_involuntary_switches: int | None
    descriptors: int | None
    pss_bytes: int | None
    private_bytes: int | None
    cpu_one_core_percent: float | None
    cpu_machine_percent: float | None


class MetricSummary(TypedDict):
    median: float
    p95: float
    max: float


type Summary = dict[str, MetricSummary | float]
type Metric = Literal[
    "cpu_one_core_percent",
    "cpu_machine_percent",
    "rss_bytes",
    "pss_bytes",
    "private_bytes",
    "threads",
    "descriptors",
]


class ProcessResult(TypedDict):
    samples: list[Sample]
    end: str | None
    summary: Summary


def read_process(process_id: int, proc: Path, ticks_per_second: int) -> Sample:
    """Read one numeric snapshot; missing optional memory/FD data remains unknown."""
    root = proc / str(process_id)
    # Names may contain spaces or parentheses; never include them in output.
    fields = (root / "stat").read_text().rsplit(")", 1)[-1].split()
    if len(fields) < 21:
        raise ValueError("Incomplete process metadata")
    if fields[0] in {"Z", "X"}:
        raise ProcessLookupError("Process exited")
    status: dict[str, int] = {}
    for line in (root / "status").read_text().splitlines():
        key, _, value = line.partition(":")
        if key in {"VmRSS", "VmHWM", "voluntary_ctxt_switches", "nonvoluntary_ctxt_switches"}:
            status[key] = int(value.split()[0])
    sample: Sample = {
        "identity": int(fields[19]),
        "cpu_seconds": (int(fields[11]) + int(fields[12])) / ticks_per_second,
        "elapsed_seconds": 0,
        "rss_bytes": status.get("VmRSS", 0) * 1024,
        "peak_rss_bytes": status.get("VmHWM", 0) * 1024,
        "virtual_bytes": int(fields[20]),
        "threads": int(fields[17]),
        "main_thread_voluntary_switches": status.get("voluntary_ctxt_switches"),
        "main_thread_involuntary_switches": status.get("nonvoluntary_ctxt_switches"),
        "descriptors": None,
        "pss_bytes": None,
        "private_bytes": None,
        "cpu_one_core_percent": None,
        "cpu_machine_percent": None,
    }
    try:
        sample["descriptors"] = sum(1 for _ in (root / "fd").iterdir())
    except OSError:
        pass  # Optional metadata may be inaccessible even while the process is alive.
    try:
        memory: dict[str, int] = {}
        for line in (root / "smaps_rollup").read_text().splitlines():
            key, _, value = line.partition(":")
            if key in {"Pss", "Private_Clean", "Private_Dirty", "Private_Hugetlb"}:
                memory[key] = int(value.split()[0]) * 1024
        sample["pss_bytes"] = memory.get("Pss")
        sample["private_bytes"] = sum(
            memory.get(key, 0) for key in ("Private_Clean", "Private_Dirty", "Private_Hugetlb")
        )
    except OSError:
        pass  # Keep unknown memory distinct from zero memory.
    return sample


def summarize(samples: Sequence[Sample], logical_cpus: int) -> Summary:
    """Report nearest-rank p95 and CPU deltas, excluding unknown measurements."""
    summary: Summary = {}
    metrics: tuple[Metric, ...] = (
        "cpu_one_core_percent",
        "cpu_machine_percent",
        "rss_bytes",
        "pss_bytes",
        "private_bytes",
        "threads",
        "descriptors",
    )
    for key in metrics:
        values = sorted(value for sample in samples if (value := sample[key]) is not None)
        if values:
            summary[key] = {
                "median": statistics.median(values),
                "p95": values[max(0, (95 * len(values) + 99) // 100 - 1)],
                "max": values[-1],
            }
    if len(samples) >= 2:
        elapsed = samples[-1]["elapsed_seconds"] - samples[0]["elapsed_seconds"]
        cpu = samples[-1]["cpu_seconds"] - samples[0]["cpu_seconds"]
        if elapsed > 0 and cpu >= 0:
            summary["cpu_time_seconds"] = cpu
            summary["average_cpu_one_core_percent"] = 100 * cpu / elapsed
            summary["average_cpu_machine_percent"] = 100 * cpu / elapsed / logical_cpus
    return summary


def record(
    pids: Sequence[int],
    seconds: float,
    interval: float,
    logical_cpus: int,
    read: Callable[[int], Sample],
    now: Callable[[], float],
    sleep: Callable[[float], None],
) -> dict[int, ProcessResult]:
    """Keep each PID's identity stable and stop on exit, reuse, or invalid counters."""
    processes: dict[int, ProcessResult] = {
        pid: {"samples": [], "end": None, "summary": {}} for pid in dict.fromkeys(pids)
    }
    previous: dict[int, Sample] = {}
    started = now()
    deadline = started + seconds
    while True:
        for process_id, result in processes.items():
            if result["end"] is not None:
                continue
            try:
                sample = read(process_id)
                sample["elapsed_seconds"] = now() - started
                before = previous.get(process_id)
                if before is not None:
                    if before["identity"] != sample["identity"]:
                        result["end"] = "PID reused"
                        continue
                    cpu = sample["cpu_seconds"] - before["cpu_seconds"]
                    if cpu < 0:
                        result["end"] = "CPU counter regressed"
                        continue
                    elapsed = sample["elapsed_seconds"] - before["elapsed_seconds"]
                    if elapsed > 0:
                        sample["cpu_one_core_percent"] = 100 * cpu / elapsed
                        sample["cpu_machine_percent"] = 100 * cpu / elapsed / logical_cpus
                previous[process_id] = sample
                result["samples"].append(sample)
            except (OSError, ValueError) as error:
                result["end"] = type(error).__name__
        remaining = deadline - now()
        if remaining <= 0 or all(result["end"] is not None for result in processes.values()):
            break
        sleep(min(interval, remaining))
    for result in processes.values():
        result["summary"] = summarize(result["samples"], logical_cpus)
    return processes


class Arguments(argparse.Namespace):
    def __init__(self) -> None:
        super().__init__()
        self.pid: list[int] = []
        self.label: str = ""
        self.seconds: float = 30
        self.interval: float = 1
        self.output: Path = Path(".")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--pid",
        type=int,
        action="append",
        required=True,
        help="repeat for app and worker; each is reported separately",
    )
    parser.add_argument("--label", required=True, help="phase, e.g. idle-hidden or recording")
    parser.add_argument("--seconds", type=float)
    parser.add_argument("--interval", type=float)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(namespace=Arguments())
    if (
        platform.system() != "Linux"
        or not math.isfinite(args.seconds)
        or args.seconds <= 0
        or not math.isfinite(args.interval)
        or args.interval < 0.1
        or any(pid <= 0 for pid in args.pid)
    ):
        parser.error(
            "requires Linux, positive PIDs, finite duration > 0 and interval >= 0.1 seconds"
        )
    ticks = int(os.sysconf("SC_CLK_TCK"))
    cores = os.cpu_count() or 1
    started = time.monotonic()
    processes = record(
        args.pid,
        args.seconds,
        args.interval,
        cores,
        lambda pid: read_process(pid, Path("/proc"), ticks),
        time.monotonic,
        time.sleep,
    )
    report = {
        "label": args.label,
        "kernel": platform.release(),
        "architecture": platform.machine(),
        "logical_cpus": cores,
        "duration_seconds": time.monotonic() - started,
        "processes": processes,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Wrote {args.output}; 100% CPU means one logical core.")


if __name__ == "__main__":
    main()
