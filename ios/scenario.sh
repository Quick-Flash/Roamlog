#!/usr/bin/env bash
# Drives Roamlog in a booted simulator with idb taps and swipes, screenshotting each step.
# Fails when a step leaves the screen unchanged: that input did nothing.
# Usage: ios/scenario.sh UDID OUT_DIR   (app installed and launched; idb connected)
# Coordinates are points on a 402x874 pt iPhone (iPhone 16 Pro / 17 class).
set -euo pipefail
udid="$1"
out="$2"
mkdir -p "$out"
previous=""
failed=0

step() {
    local name="$1"
    shift
    "$@"
    local shot="$out/$name.png"
    # A wait proves nothing by changing the screen; only inputs must. CI frame times vary,
    # so an input gets up to 6 s to show, then 1 s more to settle before the kept shot.
    if [[ "$1" == sleep || -z "$previous" ]]; then
        sleep 2
        xcrun simctl io "$udid" screenshot "$shot" > /dev/null
        echo "step '$name' done"
        previous="$shot"
        return
    fi
    local changed=0
    for _ in $(seq 12); do
        sleep 0.5
        xcrun simctl io "$udid" screenshot "$shot" > /dev/null
        if ! cmp -s "$previous" "$shot"; then
            changed=1
            break
        fi
    done
    if [[ "$changed" == 1 ]]; then
        sleep 1
        xcrun simctl io "$udid" screenshot "$shot" > /dev/null
        echo "step '$name' changed the screen"
    else
        echo "::error::step '$name' did not change the screen"
        failed=1
    fi
    previous="$shot"
}

# First frame can take 40 s on CI (Metal shader compiles); black screenshots are ~70 KB.
# Launching again only brings the running app to the front (it can start behind the home screen).
wait_for_ui() {
    local shot="$out/waiting.png"
    xcrun simctl launch "$udid" dev.traveler > /dev/null
    for _ in $(seq 90); do
        xcrun simctl io "$udid" screenshot "$shot" > /dev/null
        if [[ $(wc -c < "$shot") -gt 150000 ]]; then
            rm "$shot"
            return
        fi
        sleep 2
    done
    echo "::error::app never drew its UI"
    exit 1
}

# Walk east along fixture streets at 6 m/s (~60 s), starting with the trip.
walk() {
    tap 314 729
    xcrun simctl location "$udid" start --speed=6 \
        45.5158,-122.6788 45.5158,-122.6765 45.5168,-122.6765 45.5168,-122.6785
}

tap() { idb ui tap --udid "$udid" "$1" "$2"; }
# Bottom tabs (x at y=808): Map, History, Friends, Settings.
map_tab() { tap 55 808; }
history_tab() { tap 152 808; }
friends_tab() { tap 250 808; }
settings_tab() { tap 347 808; }
# GPX is written on the journal worker before the picker slides up.
export_gpx() {
    tap 326 737
    sleep 5
}
swipe() { idb ui swipe --udid "$udid" "$1" "$2" "$3" "$4" --duration 0.4; }

wait_for_ui
step start true
step zoom-in tap 368 535
step pan-map swipe 120 450 300 520
step category-open tap 131 729
step category-close tap 131 729
step history history_tab
step history-scroll swipe 200 650 200 250
step settings settings_tab
step friends friends_tab
step back-to-map map_tab
# Recording: CI grants location permission before launch.
# Home screen mid-trip: background location must keep recording.
background_app() {
    date +%s000 > "$out/background-start-ms"
    idb ui button --udid "$udid" HOME
}
foreground_app() {
    date +%s000 > "$out/background-end-ms"
    xcrun simctl launch "$udid" dev.traveler > /dev/null
}

step start-trip walk
step recording sleep 10
step background background_app
step backgrounded sleep 20
step foreground foreground_app
step finish-trip tap 353 729
step history-after history_tab
# Files: GPX export writes a temp file, then shows the system export picker.
step settings-again settings_tab
step gpx-export export_gpx
step picker-dismiss swipe 201 120 201 820
# Text entry: trip editor of the newest trip.
step history-again history_tab
step edit-trip tap 337 355
step name-field tap 197 317
# First keyboard use shows a swipe-typing tip over the keys; Continue dismisses it.
# Without the tip this lands on the space bar, so the name check ignores spaces.
step keyboard-tip tap 200 825
step type-name idb ui text --udid "$udid" "Lunch loop"
step save-name tap 197 452
# Sharing: identity from Keychain, iroh node, notification prompt, pairing invite.
step close-editor tap 370 132
step friends-again friends_tab
step sharing-always tap 314 256
step notification-prompt sleep 3
step allow-notifications tap 271 515
step add-friend tap 321 98
# Invitation QR: needs the iroh endpoint bound with the Keychain identity.
step pairing-name tap 200 233
step type-pairing-name idb ui text --udid "$udid" "Sim"
step show-code tap 106 282
step invite-code sleep 10
step back-from-pairing tap 37 99
# Invitation link opened from outside (camera app, messages): Roamlog shows pairing.
step open-link xcrun simctl openurl "$udid" "roamlog://pair/scenario-test"
step confirm-link tap 276 472

# iOS ending the app mid-trip: start a second trip, kill the process, reopen.
step map-again map_tab
step start-second-trip walk
step recording-second sleep 8
xcrun simctl terminate "$udid" dev.traveler
sleep 3
wait_for_ui
step reopened sleep 5
# Light theme: status bar text must turn dark (checked in screenshot).
step settings-last settings_tab
step light-theme tap 109 187

# The new trip (last in journal) must hold moving fixes and a finish.
container="$(xcrun simctl get_app_container "$udid" dev.traveler data)"
if grep -q '<gpx' "$container/tmp/roamlog-trips.gpx" 2> /dev/null; then
    echo "GPX export written"
else
    echo "::error::GPX export file missing"
    failed=1
fi
state="$container/Library/Application Support/roamlog/sharing/location-state.json"
cp "$state" "$out/location-state.json" 2> /dev/null || true
if grep -q '"always"' "$state" 2> /dev/null; then
    echo "sharing mode saved: always"
else
    echo "::error::sharing mode Always not saved"
    failed=1
fi
journal="$container/Library/Application Support/roamlog/trips.jsonl"
cp "$journal" "$out/trips.jsonl"
python3 - "$out/trips.jsonl" <<'PY' || failed=1
import json, sys
events = [json.loads(line) for line in open(sys.argv[1]) if line.strip()]
starts = [i for i, e in enumerate(events) if e.get("op") == "start"]
# Seed journal holds 10 trips; the scenario's own trip must come after them.
if len(starts) <= 10:
    print("::error::scenario recorded no trip of its own")
    sys.exit(1)
if len(starts) < 12:
    print("::error::scenario's second (killed) trip never started")
    sys.exit(1)
trip = events[starts[10]:starts[11]]
killed = events[starts[11]:]
fixes = [e["fix"] for e in trip if e.get("op") == "fix"]
lons = {round(f["lon"], 5) for f in fixes}
print(f"recorded trip: {len(fixes)} fixes, {len(lons)} distinct longitudes, ops {sorted({e['op'] for e in trip})}")
if len(fixes) < 5 or len(lons) < 3 or not any(e.get("op") == "finish" for e in trip):
    print("::error::recorded trip lacks moving fixes or finish")
    sys.exit(1)
# Typed with the on-screen keyboard in the trip editor.
if not any(e.get("op") == "rename_trip" and e.get("name", "").strip() == "Lunch loop" for e in trip):
    print("::error::typed trip name never reached the journal")
    sys.exit(1)
print("typed trip name saved")
# Fixes recorded while Roamlog sat behind the home screen.
out = sys.argv[1].rsplit("/", 1)[0]
since, until = (int(open(f"{out}/background-{edge}-ms").read()) for edge in ("start", "end"))
background = [f for f in fixes if since + 2000 <= f["timestamp_ms"] <= until - 2000]
print(f"background recording: {len(background)} fixes in {(until - since) // 1000} s behind home screen")
if len(background) < 5:
    print("::error::recording stopped while Roamlog was in the background")
    sys.exit(1)
# Process ended mid-trip: reopening pauses the trip at its last fix, never finishes it.
killed_ops = [e["op"] for e in killed]
killed_fixes = [e["fix"] for e in killed if e.get("op") == "fix"]
print(f"killed trip: {len(killed_fixes)} fixes, ops after start {killed_ops[1:][-3:]}")
pause = next((e for e in killed if e.get("op") == "pause"), None)
if not killed_fixes or pause is None or "finish" in killed_ops:
    print("::error::killed trip was not paused on reopen")
    sys.exit(1)
if pause["timestamp_ms"] != killed_fixes[-1]["timestamp_ms"]:
    print("::error::killed trip paused at reopen time, not at its last fix")
    sys.exit(1)
print("killed trip paused at its last fix")
PY
exit "$failed"
