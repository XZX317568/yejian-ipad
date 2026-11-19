#!/usr/bin/env bash
# Run on the GitHub macOS runner after building the simulator application.
set -euo pipefail
APP="${1:?path to Yejian.app is required}"
OUT="${2:?artifact directory is required}"
BUNDLE=app.yejian.reader.rust
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
DEVICE=$(xcrun simctl list devices available -j | python3 -c '
import json, sys
devices = [d for runtime in json.load(sys.stdin)["devices"].values() for d in runtime]
ipads = [d for d in devices if "iPad" in d["name"]]
preferred = next((d for d in ipads if "11-inch" in d["name"] and "Pro" in d["name"]), ipads[0])
print(preferred["udid"])')
CONTAINER=""
LAUNCH_PID=""

collect() {
  local status=$?
  trap - EXIT
  if [ -n "$CONTAINER" ] && [ -f "$CONTAINER/Documents/yejian-startup.log" ]; then
    cp "$CONTAINER/Documents/yejian-startup.log" "$OUT/startup-last.log"
  fi
  xcrun simctl spawn "$DEVICE" log show --style compact --last 10m \
    --predicate 'process == "Yejian"' > "$OUT/system-log.txt" 2>&1 || true
  if [ "$status" -ne 0 ]; then
    xcrun simctl io "$DEVICE" screenshot "$OUT/failure.png" || true
    tail -n 100 "$OUT/"*.log || true
  fi
  xcrun simctl terminate "$DEVICE" "$BUNDLE" 2>/dev/null || true
  if [ -n "$LAUNCH_PID" ]; then wait "$LAUNCH_PID" 2>/dev/null || true; fi
  xcrun simctl shutdown "$DEVICE" || true
  exit "$status"
}
trap collect EXIT
xcrun simctl boot "$DEVICE"
xcrun simctl bootstatus "$DEVICE" -b
xcrun simctl status_bar "$DEVICE" override --time 9:41 --batteryState charged --batteryLevel 100
xcrun simctl install "$DEVICE" "$APP"
CONTAINER=$(xcrun simctl get_app_container "$DEVICE" "$BUNDLE" data)
printf '%s\n' "$DEVICE" > "$OUT/device.txt"

shoot() {
  local name=$1
  local screen=${name%%-*}
  shift
  # Each screen must acknowledge this launch, never reuse an earlier marker.
  local marker="$CONTAINER/Documents/Yejian/ui-snapshots/$screen.json"
  if [ -f "$marker" ]; then rm "$marker"; fi
  xcrun simctl launch --console --terminate-running-process "$DEVICE" \
    "$BUNDLE" --ui-snapshot "$screen" "$@" > "$OUT/$name-console.log" 2>&1 &
  LAUNCH_PID=$!
  local ready=0
  for ((attempt=0; attempt<90; attempt++)); do
    if ! kill -0 "$LAUNCH_PID" 2>/dev/null; then
      echo "::error::Yejian exited before rendering $name"
      return 1
    fi
    if [ -f "$marker" ]; then ready=1; break; fi
    sleep 2
  done
  if [ "$ready" -ne 1 ]; then
    echo "::error::Yejian did not finish rendering $name within 180 seconds"
    return 1
  fi
  cp "$marker" "$OUT/$name.json"
  cp "$CONTAINER/Documents/yejian-startup.log" "$OUT/$name-startup.log"
  # One extra compositor turn after the Rust UI's readiness acknowledgement.
  sleep 1
  kill -0 "$LAUNCH_PID"
  xcrun simctl io "$DEVICE" screenshot "$OUT/$name.png"
  xcrun simctl terminate "$DEVICE" "$BUNDLE"
  wait "$LAUNCH_PID" 2>/dev/null || true
  LAUNCH_PID=""
}
shoot shelf --seed-demo
shoot detail
shoot reader
shoot history
shoot sources
shoot settings
shoot community
shoot shelf-landscape --ui-landscape
shoot detail-landscape --ui-landscape
shoot reader-landscape --ui-landscape
echo "PASS: iPad rendered all production screens, rotated to landscape, and fetched the live Aidoku community repository"
