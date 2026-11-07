#!/usr/bin/env python3
"""Runner-independent §1.5 launch gate (DECISIONS.md D-0107).

Reads the measure-launch.sh JSON files of one CI job and writes target/ios/ios-metrics.json.
Fails when CallXL's warm median exceeds the bare LaunchBaseline's warm median (same job,
same Simulator) by more than BUDGET_MS. An absolute CallXL median above 1000 ms is only a
warning: the shared runner's speed varies about 2x between jobs.
Usage: packaging/ios/launch-gate.py <dir with launch-*.json> <out ios-metrics.json>
"""
import json
import pathlib
import sys

BUDGET_MS = 700
ABSOLUTE_WARN_MS = 1000

src, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
runs = {p.stem.removeprefix("launch-"): json.loads(p.read_text()) for p in sorted(src.glob("launch-*.json"))}
base = runs["LaunchBaseline"]["warm_median_ms"]
app = runs["CallXL"]["warm_median_ms"]
delta = app - base
metrics = {
    "launch_simctl_release_warm_median_ms": {k: v["warm_median_ms"] for k, v in runs.items()},
    "launch_simctl_release_cold_ms": {k: v["cold_ms"] for k, v in runs.items()},
    "callxl_minus_baseline_ms": delta,
    "budget_ms": BUDGET_MS,
    "runs": runs,
}
out.write_text(json.dumps(metrics, indent=2))
print(f"CallXL {app:.0f} ms, LaunchBaseline {base:.0f} ms, overhead {delta:.0f} ms (budget {BUDGET_MS} ms)")
if app > ABSOLUTE_WARN_MS:
    print(f"::warning::CallXL launch to Home {app:.0f} ms > {ABSOLUTE_WARN_MS} ms on this runner (baseline {base:.0f} ms)")
if delta > BUDGET_MS:
    sys.exit(f"CallXL launch overhead {delta:.0f} ms over the bare app exceeds {BUDGET_MS} ms")
