#!/usr/bin/env bash
# §1.5 launch check on the iOS Simulator without XCUITest: installs the Release
# CallXL.app, launches it with `simctl launch` (1 cold + 5 warm launches, real core) and
# reads the launch phases the app writes to Documents/launch.txt when started with
# -uiTestReportLaunch ("<ms to Home> app.init=<ms> home.appear=<ms>", from process start).
# Fails when the median warm launch to Home is 1000 ms or more.
# Usage: packaging/ios/measure-launch.sh <simulator-udid> <path/to/CallXL.app>
set -euo pipefail
UDID="$1"
APP="$2"
BUNDLE=com.callxl.app

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

python3 - "${results[@]}" <<'PY'
import statistics, sys
v = [float(x) for x in sys.argv[1:]]
warm = v[1:]
med = statistics.median(warm)
print(f"launch-to-home-ms (simctl, Release): cold={v[0]:.0f} warm median={med:.0f} warm={[round(x) for x in warm]}")
sys.exit(0 if med < 1000 else f"launch to Home {med:.0f} ms >= 1000 ms")
PY
