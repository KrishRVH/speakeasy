#!/usr/bin/env python3
"""Sample explicit app/worker PIDs without reading command lines or user content."""
import argparse
import json
import os
from pathlib import Path
import platform
import statistics
import time


def read_process(process_id):
    root = Path("/proc") / str(process_id)
    # The command name may contain spaces or parentheses; never include it in output.
    fields = (root / "stat").read_text().rsplit(")", 1)[1].split()
    if fields[0] in {"Z", "X"}:
        raise ProcessLookupError("Process exited")
    status = {}
    for line in (root / "status").read_text().splitlines():
        key, _, value = line.partition(":")
        if key in {"VmRSS", "VmHWM", "voluntary_ctxt_switches", "nonvoluntary_ctxt_switches"}:
            status[key] = int(value.split()[0])
    sample = {
        "identity": int(fields[19]),
        "cpu_seconds": (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK"),
        "rss_bytes": status.get("VmRSS", 0) * 1024,
        "peak_rss_bytes": status.get("VmHWM", 0) * 1024,
        "virtual_bytes": int(fields[20]),
        "threads": int(fields[17]),
        "main_thread_voluntary_switches": status.get("voluntary_ctxt_switches"),
        "main_thread_involuntary_switches": status.get("nonvoluntary_ctxt_switches"),
    }
    try:
        sample["descriptors"] = len(list((root / "fd").iterdir()))
    except OSError:
        sample["descriptors"] = None
    try:
        memory = {}
        for line in (root / "smaps_rollup").read_text().splitlines():
            key, _, value = line.partition(":")
            if key in {"Pss", "Private_Clean", "Private_Dirty", "Private_Hugetlb"}:
                memory[key] = int(value.split()[0]) * 1024
        sample["pss_bytes"] = memory.get("Pss")
        sample["private_bytes"] = sum(memory.get(key, 0) for key in ("Private_Clean", "Private_Dirty", "Private_Hugetlb"))
    except OSError:
        sample["pss_bytes"] = sample["private_bytes"] = None
    return sample


def summarize(samples):
    summary = {}
    for key in ("cpu_one_core_percent", "cpu_machine_percent", "rss_bytes", "pss_bytes", "private_bytes", "threads", "descriptors"):
        values = sorted(sample[key] for sample in samples if sample.get(key) is not None)
        if values:
            summary[key] = {"median": statistics.median(values), "p95": values[max(0, (95 * len(values) + 99) // 100 - 1)], "max": values[-1]}
    if len(samples) >= 2:
        elapsed = samples[-1]["elapsed_seconds"] - samples[0]["elapsed_seconds"]
        cpu = samples[-1]["cpu_seconds"] - samples[0]["cpu_seconds"]
        summary["cpu_time_seconds"] = cpu
        summary["average_cpu_one_core_percent"] = 100 * cpu / elapsed
        summary["average_cpu_machine_percent"] = 100 * cpu / elapsed / (os.cpu_count() or 1)
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pid", type=int, action="append", required=True, help="repeat for app and worker; each is reported separately")
    parser.add_argument("--label", required=True, help="phase, e.g. idle-hidden or recording")
    parser.add_argument("--seconds", type=float, default=30)
    parser.add_argument("--interval", type=float, default=1)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if platform.system() != "Linux" or args.seconds <= 0 or args.interval < 0.1 or any(pid <= 0 for pid in args.pid):
        parser.error("requires Linux, positive PIDs/duration and interval >= 0.1 seconds")
    processes = {pid: {"samples": [], "end": None} for pid in dict.fromkeys(args.pid)}
    previous = {}
    started = time.monotonic()
    deadline = started + args.seconds
    while True:
        for process_id, result in processes.items():
            if result["end"] is not None:
                continue
            try:
                sample = read_process(process_id)
                now = time.monotonic()
                sample["elapsed_seconds"] = now - started
                before = previous.get(process_id)
                if before and before["identity"] != sample["identity"]:
                    result["end"] = "PID reused"
                    continue
                if before:
                    sample["cpu_one_core_percent"] = 100 * (sample["cpu_seconds"] - before["cpu_seconds"]) / (now - started - before["elapsed_seconds"])
                    sample["cpu_machine_percent"] = sample["cpu_one_core_percent"] / (os.cpu_count() or 1)
                previous[process_id] = sample
                result["samples"].append(sample)
            except OSError as error:
                result["end"] = type(error).__name__
        remaining = deadline - time.monotonic()
        if remaining <= 0 or all(result["end"] for result in processes.values()):
            break
        time.sleep(min(args.interval, remaining))
    for result in processes.values():
        result["summary"] = summarize(result["samples"])
    report = {"label": args.label, "kernel": platform.release(), "architecture": platform.machine(), "logical_cpus": os.cpu_count(), "duration_seconds": time.monotonic() - started, "processes": processes}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Wrote {args.output}; 100% CPU means one logical core.")


if __name__ == "__main__":
    main()
