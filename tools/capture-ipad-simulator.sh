#!/usr/bin/env bash
# Run on the GitHub macOS runner after building the simulator application.
set -euo pipefail
APP="${1:?path to Yejian.app is required}"
OUT="${2:?artifact directory is required}"
BUILD="${3:?Xcode derived data directory is required}"
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
  # XCTest can reinstall the app and change its container UUID.
  CONTAINER=$(xcrun simctl get_app_container "$DEVICE" "$BUNDLE" data 2>/dev/null || true)
  if [ -n "$CONTAINER" ] && [ -f "$CONTAINER/Documents/yejian-startup.log" ]; then
    cp "$CONTAINER/Documents/yejian-startup.log" "$OUT/startup-last.log"
  fi
  xcrun simctl spawn "$DEVICE" log show --style compact --last 10m \
    --predicate 'process == "Yejian"' > "$OUT/system-log.txt" 2>&1 || true
  if [ "$status" -ne 0 ]; then
    xcrun simctl io "$DEVICE" screenshot "$OUT/failure.png" || true
    # Do not tail capture.log: this function's output is still being piped
    # into that file by the workflow.
    for log in "$OUT/"*-console.log "$OUT/startup-last.log" "$OUT/rotation-tests.log"; do
      if [ -f "$log" ]; then tail -n 80 "$log" || true; fi
    done
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
  local run_id
  run_id=$(python3 -c 'import uuid; print(uuid.uuid4())')
  shift
  # Each screen must acknowledge this launch, never reuse an earlier marker.
  local marker="$CONTAINER/Documents/Yejian/ui-snapshots/$screen.json"
  if [ -f "$marker" ]; then rm "$marker"; fi
  xcrun simctl launch --console --terminate-running-process "$DEVICE" \
    "$BUNDLE" --ui-snapshot "$screen" --ui-snapshot-run "$run_id" "$@" > "$OUT/$name-console.log" 2>&1 &
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
  python3 - "$OUT/$name.json" "$run_id" <<'PY'
import json, sys
with open(sys.argv[1]) as stream:
    report = json.load(stream)
if report.get("run_id") != sys.argv[2]:
    raise SystemExit("Readiness belongs to an earlier launch")
PY
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
# Reproduce the format written by build 3, then let XCTest reinstall the app.
# The five books must survive using the real production Library::open path.
python3 - "$CONTAINER/Documents/Yejian" "$OUT/legacy-library.json" <<'PY'
import json, pathlib, sqlite3, sys
root = pathlib.Path(sys.argv[1])
with sqlite3.connect(root / "library.sqlite3") as db:
    rows = db.execute("SELECT id, archive_path, cover_path FROM books ORDER BY id").fetchall()
    if len(rows) != 5:
        raise SystemExit("Expected the five imported demo books")
    for book_id, archive, cover in rows:
        paths = [root / archive, root / cover]
        if not all(path.is_file() for path in paths):
            raise SystemExit("Cannot seed legacy paths: fixture files are missing")
        db.execute("UPDATE books SET archive_path=?, cover_path=? WHERE id=?",
                   (str(paths[0]), str(paths[1]), book_id))
    db.execute("PRAGMA user_version=3")
pathlib.Path(sys.argv[2]).write_text(json.dumps({
    "root": str(root), "book_ids": [row[0] for row in rows], "schema": 3
}), encoding="utf-8")
PY
# Exercise real device rotation with XCTest. requestGeometryUpdate from inside
# an app is rejected in iPad multitasking mode, so it is not a rotation test.
xcodebuild -project rust-reader/platform/ios/Yejian.xcodeproj -scheme Yejian \
  -configuration Debug -sdk iphonesimulator -destination "id=$DEVICE" \
  -derivedDataPath "$BUILD" -parallel-testing-enabled NO \
  -maximum-concurrent-test-simulator-destinations 1 \
  test-without-building CODE_SIGN_IDENTITY=- 2>&1 | tee "$OUT/rotation-tests.log"
RUNNER_CONTAINER=$(xcrun simctl get_app_container "$DEVICE" app.yejian.reader.uitests.xctrunner data)
for screen in shelf detail reader; do
  # XCTest captures the live app's acknowledgement and its screenshot
  # together. Both belong to the runner, independent of app-container UUIDs.
  cp "$RUNNER_CONTAINER/Documents/$screen-landscape.png" "$OUT/$screen-landscape.png"
  cp "$RUNNER_CONTAINER/Documents/$screen-landscape.json" "$OUT/$screen-landscape.json"
done
CONTAINER=$(xcrun simctl get_app_container "$DEVICE" "$BUNDLE" data)
python3 - "$CONTAINER/Documents/Yejian" "$OUT/relocated-library.json" <<'PY'
import json, pathlib, sqlite3, sys
root = pathlib.Path(sys.argv[1])
with sqlite3.connect(f"file:{root / 'library.sqlite3'}?mode=ro", uri=True) as db:
    rows = db.execute("SELECT id, archive_path, cover_path FROM books ORDER BY id").fetchall()
    version = db.execute("PRAGMA user_version").fetchone()[0]
pathlib.Path(sys.argv[2]).write_text(json.dumps({
    "root": str(root), "book_ids": [row[0] for row in rows], "schema": version,
    "managed_paths_relative": all(
        archive.startswith("archives/") and cover.startswith("covers/")
        for _, archive, cover in rows
    )
}), encoding="utf-8")
PY
echo "Captured iPad screens; the following workflow step must validate the complete evidence."
