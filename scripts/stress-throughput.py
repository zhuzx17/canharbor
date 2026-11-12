#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Run precompiled virtual-only throughput tests sequentially, including failures.

Build outside this runner so compiler CPU is excluded. Optional baseline binaries
are alternated with current binaries; no retries, priority changes or CAN access.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import resource
import subprocess
import time

TESTS = {
    "lane": "p2_tests::controlled_virtual_lane_throughput",
    "verifier": "tracking::performance::verifier_throughput_matrix",
    "recorded": "recorded_high_rate_virtual_run_has_no_unreported_rx_loss",
}


def measurement_status(case, rows, lane, output):
    if case == "lane":
        complete = bool(lane) and "submitted=40000 peer_rx=40000" in output
        return complete, complete and int(lane[2]) >= 40000
    if case == "verifier":
        pairs = {(r.get("stages"), r.get("sliding")) for r in rows}
        complete = len(rows) == 4 and pairs == {(1, False), (1, True), (64, False), (64, True)}
        return complete, complete and all(r.get("frames") == 640000 for r in rows)
    pairs = {(r.get("sliding"), r.get("queues")) for r in rows}
    complete = len(rows) == 4 and pairs == {(False, False), (False, True), (True, False), (True, True)}
    passed = complete and all(r.get("submitted") == 10000 and r.get("peer_rx") == 10000 and r.get("skipped") == 0 and r.get("verdict") == "pass" and r.get("recovery") == "verified" for r in rows)
    return complete, passed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for case in TESTS:
        parser.add_argument(f"--{case}", type=Path, required=True)
        parser.add_argument(f"--baseline-{case}", type=Path)
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not 1 <= args.rounds <= 10:
        parser.error("rounds must be in 1..10")
    variants = {"current": {case: getattr(args, case).resolve() for case in TESTS}}
    baselines = [getattr(args, f"baseline_{case}") for case in TESTS]
    if any(baselines):
        if not all(baselines):
            parser.error("all three baseline binaries must be supplied together")
        variants["baseline"] = {case: p.resolve() for case, p in zip(TESTS, baselines)}
    identities = {}
    for label, cases in variants.items():
        identities[label] = {}
        for case, binary in cases.items():
            listing = subprocess.run([str(binary), "--list"], capture_output=True, text=True, check=True)
            if f"{TESTS[case]}: test" not in listing.stdout:
                parser.error(f"{binary} does not contain {TESTS[case]}")
            digest = hashlib.sha256()
            with binary.open("rb") as source:
                for chunk in iter(lambda: source.read(1024 * 1024), b""):
                    digest.update(chunk)
            identities[label][case] = {"path": str(binary), "sha256": digest.hexdigest()}
    report = {"format": "canharbor-virtual-throughput-v1", "created_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "thresholds": {"lane_fps": 40000, "recorded_fps": 10000, "recorded_submitted": 10000, "skipped": 0}, "scope": "virtual backend only; wall gates are not replaced by CPU-normalized rates; compilation excluded", "binaries": identities, "runs": []}
    for round_index in range(args.rounds):
        labels = list(variants) if round_index % 2 == 0 else list(reversed(variants))
        for case, test in TESTS.items():
            for label in labels:
                before = resource.getrusage(resource.RUSAGE_CHILDREN)
                started = time.monotonic()
                try:
                    result = subprocess.run([str(variants[label][case]), test, "--ignored", "--exact", "--nocapture", "--test-threads=1"], capture_output=True, text=True, timeout=180, env=os.environ.copy())
                except subprocess.TimeoutExpired as error:
                    def decoded(value):
                        return value.decode("utf8", errors="replace") if isinstance(value, bytes) else value or ""
                    result = subprocess.CompletedProcess([], 124, decoded(error.stdout), decoded(error.stderr) + "\nrunner timeout after 180s")
                wall = time.monotonic() - started
                after = resource.getrusage(resource.RUSAGE_CHILDREN)
                cpu = (after.ru_utime + after.ru_stime) - (before.ru_utime + before.ru_stime)
                output = result.stdout + result.stderr
                rows = [json.loads(value) for value in re.findall(r"(?:VERIFIER|THROUGHPUT) (\{[^\n]+\})", output)]
                lane = re.search(r"elapsed_seconds=([\d.]+) fps=(\d+)", output)
                row = {"round": round_index + 1, "variant": label, "case": case, "exit_code": result.returncode, "wall_seconds": wall, "cpu_seconds": cpu, "cpu_over_wall": cpu / wall, "involuntary_context_switches": after.ru_nivcsw - before.ru_nivcsw, "voluntary_context_switches": after.ru_nvcsw - before.ru_nvcsw, "measurements": rows, "stdout_stderr": output}
                row["complete_measurements"], row["gates_met"] = measurement_status(case, rows, lane, output)
                if lane:
                    row["lane_seconds"] = float(lane[1])
                    row["lane_fps"] = int(lane[2])
                report["runs"].append(row)
                args.output.parent.mkdir(parents=True, exist_ok=True)
                args.output.write_text(json.dumps(report, indent=2) + "\n")
                print(json.dumps({k: v for k, v in row.items() if k != "stdout_stderr"}), flush=True)
    return int(any(r["exit_code"] or not r["complete_measurements"] or not r["gates_met"] for r in report["runs"]))


if __name__ == "__main__":
    raise SystemExit(main())
