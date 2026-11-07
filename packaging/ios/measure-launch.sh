#!/usr/bin/env bash
# §1.5 launch check on the iOS Simulator without XCUITest: installs the Release
# CallXL.app, launches it with `simctl launch` (1 cold + 5 warm launches, real core) and
# reads the launch phases the app writes to Documents/launch.txt when started with
# -uiTestReportLaunch ("<ms to Home> app.init=<ms> home.appear=<ms>", from process start).
# Fails when the median warm launch to Home is 1000 ms or more.
# With a third argument (`--report-only`) it only prints (used for LaunchBaseline, the
# Simulator's floor for a bare SwiftUI app).
# Usage: packaging/ios/measure-launch.sh <simulator-udid> <path/to/X.app> [--report-only]
set -euo pipefail
UDID="$1"
APP="$2"
REPORT_ONLY="${3:-}"
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

python3 - "$BUNDLE" "$REPORT_ONLY" "${results[@]}" <<'PY'
import statistics, sys
bundle, report_only = sys.argv[1], sys.argv[2] == "--report-only"
v = [float(x) for x in sys.argv[3:]]
warm = v[1:]
med = statistics.median(warm)
print(f"launch-to-first-screen-ms {bundle} (simctl, Release): cold={v[0]:.0f} warm median={med:.0f} warm={[round(x) for x in warm]}")
if not report_only and med >= 1000:
    sys.exit(f"launch to Home {med:.0f} ms >= 1000 ms")
PY
