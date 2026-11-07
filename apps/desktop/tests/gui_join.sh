#!/usr/bin/env bash
# Two-process GUI e2e (M2): a headless `callxl-cli` peer creates a session; the desktop
# app (`callxl`, under Xvfb) discovers it over real mDNS and joins it through the UI's own
# "Join" command path (CALLXL_DEBUG_AUTOJOIN=1 invokes App.join-session for the first
# listed session). The CLI's libwebrtc stats must then show a connected LAN path to the
# GUI, and — when PulseAudio is available for the GUI's platform audio device — audio
# arriving from it.
#
# Usage: apps/desktop/tests/gui_join.sh [path/to/callxl] [path/to/callxl-cli]
# Env:   CALLXL_GUI_JOIN_AUDIO=1  require audio from the GUI (CI sets it; needs Pulse).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
TARGET="${CARGO_TARGET_DIR:-$ROOT/target}"
GUI="${1:-$TARGET/debug/callxl}"
CLI="${2:-$TARGET/debug/callxl-cli}"
WORK="$(mktemp -d)"
GUI_PID=""
CLI_PID=""
cleanup() {
  # xvfb-run runs in its own process group (setsid): stop it, Xvfb and the app together.
  [[ -n "$GUI_PID" ]] && kill -- "-$GUI_PID" 2>/dev/null || true
  [[ -n "$CLI_PID" ]] && kill "$CLI_PID" 2>/dev/null || true
  wait 2>/dev/null || true
  if [[ -n "${CALLXL_E2E_ARTIFACTS:-}" ]]; then
    mkdir -p "$CALLXL_E2E_ARTIFACTS/gui_join"
    cp "$WORK"/*.log "$CALLXL_E2E_ARTIFACTS/gui_join/" 2>/dev/null || true
  fi
  rm -rf "$WORK"
}
trap cleanup EXIT

# A 20 s 48 kHz mono test signal for the CLI's test audio device.
python3 - "$WORK/tone.wav" <<'EOF'
import math, struct, sys, wave
with wave.open(sys.argv[1], "wb") as w:
    w.setnchannels(1); w.setsampwidth(2); w.setframerate(48000)
    w.writeframes(b"".join(struct.pack("<h", int(8000 * math.sin(2 * math.pi * 440 * i / 48000))) for i in range(48000 * 20)))
EOF

"$CLI" --control tcp://127.0.0.1:0 --data-dir "$WORK/bob" --name Bob --no-cloud \
  --adm "test:play=$WORK/tone.wav,record=$WORK/bob-rec.wav" >"$WORK/cli.out" 2>"$WORK/cli.log" &
CLI_PID=$!
for _ in $(seq 600); do grep -q '"ready"' "$WORK/cli.out" 2>/dev/null && break; sleep 0.1; done
grep -q '"ready"' "$WORK/cli.out" || { echo "FAIL: callxl-cli did not start"; cat "$WORK/cli.log"; exit 1; }
CONTROL="$(python3 -c 'import json,sys; print(json.loads(open(sys.argv[1]).readline())["control"][6:])' "$WORK/cli.out")"
echo "callxl-cli control: $CONTROL"

# Minimal JSON-lines client for the CLI's control socket (skips pushed events).
cat >"$WORK/ctl.py" <<'EOF'
import json, socket, sys
host, port = sys.argv[1].rsplit(":", 1)
s = socket.create_connection((host, int(port)), timeout=20)
s.sendall((json.dumps({"id": 1, "cmd": sys.argv[2], "args": json.loads(sys.argv[3] if len(sys.argv) > 3 else "{}")}) + "\n").encode())
f = s.makefile()
for line in f:
    m = json.loads(line)
    if m.get("id") == 1:
        if not m.get("ok"):
            sys.exit("control error: " + m.get("error", "?"))
        print(json.dumps(m["result"]))
        break
EOF
ctl() { python3 "$WORK/ctl.py" "$CONTROL" "$@"; }

SID="$(ctl create_session '{"name":"gui-join"}' | python3 -c 'import json,sys; print(json.load(sys.stdin))')"
echo "session created by the CLI peer: $SID"

T0=$(date +%s.%N)
CALLXL_DEBUG_AUTOJOIN=1 setsid xvfb-run -a "$GUI" --data-dir "$WORK/gui" >"$WORK/gui.log" 2>&1 &
GUI_PID=$!

need_audio="${CALLXL_GUI_JOIN_AUDIO:-0}"
for i in $(seq 60); do
  sleep 1
  if ! kill -0 "$GUI_PID" 2>/dev/null; then
    echo "FAIL: the desktop app exited"; cat "$WORK/gui.log"; exit 1
  fi
  D="$(ctl diagnostics)"
  if python3 - "$D" "$need_audio" <<'EOF'
import json, sys
d = json.loads(sys.argv[1]); need_audio = sys.argv[2] == "1"
peers = d.get("peers", [])
if not peers or not peers[0].get("path"):
    sys.exit(1)
p = peers[0]
if p["send_kbps"] < 20 or (need_audio and p["recv_kbps"] < 20):
    sys.exit(1)
print("CLI sees the GUI peer: %s path, %s -> %s (%s/%s), send %.0f kbps, recv %.0f kbps, rtt %.1f ms, Opus %s ms" % (
    p["path"]["kind"], p["path"]["local_addr"], p["path"]["remote_addr"], p["path"]["local_candidate_type"],
    p["path"]["remote_candidate_type"], p["send_kbps"], p["recv_kbps"], p["rtt_ms"], p["opus_frame_ms"]))
EOF
  then
    printf 'joined from the GUI and connected %.1f s after launch\n' "$(echo "$(date +%s.%N) - $T0" | bc)"
    SNAP="$(ctl snapshot)"
    python3 - "$SNAP" <<'EOF'
import json, sys
s = json.loads(sys.argv[1])
print("CLI call state:", s["call_state"], "| members:", [p["display_name"] for p in s["peers"]])
assert s["call_state"] == "InCall", s["call_state"]
assert any(p["connected"] for p in s["peers"]), "GUI peer not connected"
EOF
    grep -E "debug autojoin" "$WORK/gui.log" || true
    echo "PASS gui_join"
    exit 0
  fi
done
echo "FAIL: no connected call with the GUI within 60 s"
echo "--- last CLI diagnostics"; echo "$D" | head -c 2000; echo
echo "--- GUI log"; tail -50 "$WORK/gui.log"
exit 1
