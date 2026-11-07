#!/usr/bin/env bash
# §1.5 launch check on the iOS Simulator without XCUITest: installs the Release
# CallXL.app, launches it with `simctl launch` (1 cold + 5 warm launches, real core) and
# reads the launch phases the app writes to Documents/launch.txt when started with
# -uiTestReportLaunch ("<ms to Home> app.init=<ms> home.appear=<ms>", from process start).
# Report only: prints the medians and writes them to <out.json>; the gate (CallXL minus
# the LaunchBaseline floor, measured in the same job) is packaging/ios/launch-gate.py.
# Usage: packaging/ios/measure-launch.sh <simulator-udid> <path/to/X.app> <out.json>
set -euo pipefail
UDID="$1"
APP="$2"
OUT_JSON="$3"
BUNDLE="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP/Info.plist")"

xcrun simctl install "$UDID" "$APP"
DATA="$(xcrun simctl get_app_container "$UDID" "$BUNDLE" data)"
results=()
for i in 0 1 2 3 4 5; do
  xcrun simctl terminate "$UDID" "$BUNDLE" >/dev/null 2>&1 || true
  rm -f "$DATA/Documents/launch.txt"
  sleep 1
  xcrun simctl launch "$UDID" "$BUNDLE" -uiTestReportLaunch >/dev/null
  for _ in $(seq 1 40); do
    [[ -s "$DATA/Documents/launch.txt" ]] && break
    sleep 0.25
  done
  line="$(cat "$DATA/Documents/launch.txt")"
  echo "launch $i ($([[ $i -eq 0 ]] && echo cold || echo warm)): $line"
  results+=("${line%% *}")
done
xcrun simctl terminate "$UDID" "$BUNDLE" >/dev/null 2>&1 || true

python3 - "$BUNDLE" "$OUT_JSON" "${results[@]}" <<'PY'
import json, statistics, sys
bundle, out = sys.argv[1], sys.argv[2]
v = [float(x) for x in sys.argv[3:]]
warm = v[1:]
med = statistics.median(warm)
print(f"launch-to-first-screen-ms {bundle} (simctl, Release): cold={v[0]:.0f} warm median={med:.0f} warm={[round(x) for x in warm]}")
json.dump({"bundle": bundle, "cold_ms": v[0], "warm_ms": warm, "warm_median_ms": med}, open(out, "w"), indent=2)
PY
