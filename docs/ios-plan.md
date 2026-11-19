Roamlog — iOS Port Plan

Status (2026-11-19): Phases 0–6 done and verified in the iOS simulator on CI;
Phase 7 (real iPhone) blocked on hardware. zgui iOS fixes live on
Quick-Flash/zgui branch `ios`. Known gaps listed at the end.

Constraints: no Mac, no Apple device. All builds and tests run on GitHub Actions
macOS runners (repo public, so macOS minutes free). Distribution through
SideStore with free Apple ID: CI ships unsigned IPA, SideStore signs on phone.
No App Review, no APNs push (Roamlog never used push; local notifications fine).

Simulator cannot prove: real GPS, background kill/relaunch, battery, Keychain
under SideStore signing, network transitions, real Metal GPU. Phase 7 needs a
real iPhone; everything before it does not.

## Current state

- Core (`model`, `geo`, `coverage`, `roads`, `pmtiles`, `mvt`, `gpx`, `store`,
  `recovery`, `area`, `sharing` protocol) is plain Rust.
- Rendering: zgui → wgpu; `default_backends()` already picks Metal on
  `target_vendor = "apple"`.
- zgui (Quick-Flash fork, pinned rev) has no iOS host or services. Android
  services live in `zgui-platform-winit/src/android/*` + `android_host.rs`
  (~3k lines).
- Known iOS compile risks in zgui: `arboard` (enabled for every non-Android
  target, no iOS support), `accesskit_winit` (no iOS adapter), winit iOS
  lifecycle (`run` never returns, must own main thread).
- Roamlog Android code: `src/android.rs` (880 lines, facade + recording
  policy), `src/sharing/android_*.rs`, ~60 `cfg(target_os = "android")` sites.
  Desktop (`not(android)`) fallbacks already exist for most UI paths.

## Phase 0 — Prep on Linux (no Apple needed)

Goal: iOS can reuse recording/sharing policy instead of copying it.

- Move platform-neutral logic out of `src/android.rs` into shared module
  (`fix`, `settle`, `share_interval`, `share_fix`, `command`, recorder event
  handling), taking plain fix values instead of `location::Update`.
- Android behavior unchanged; `cargo xtask verify` + APK smoke test on device.

Exit: `android.rs` holds only zgui Android calls and wiring.

## Phase 1 — Compile for iOS in CI

Goal: `cargo build --target aarch64-apple-ios-sim --features ui` succeeds.
No `sharing` feature yet (keeps iroh/ring/tokio out until Phase 5).

- Add `macos-latest` CI job `ios` (pull requests too; fast feedback, since
  local Linux cannot check iOS targets because `ring` and other C build
  scripts need Apple SDK).
- zgui fork: gate `arboard` to desktop targets, stub clipboard on iOS; make
  `accesskit_winit` optional or no-op on iOS; fix whatever else breaks.
- Roamlog: iOS takes existing `not(android)` paths for now.
- Bump zgui rev in `Cargo.toml` after fork changes.

Exit: CI job green for `aarch64-apple-ios-sim` and `aarch64-apple-ios`.

Findings (2026-11-18): `cargo check` passes for both targets with `ui` and no
code changes; `arboard` and `accesskit_winit` compile on iOS. Linking only
verifiable in CI. `sharing` fails: iroh's `netdev` 0.46 uses
`libc::in6_ifreq`, absent for iOS in `libc` (Phase 5).

`ios/check.sh` runs this check (both iOS targets, `sharing` on) and CI runs it
on every push in the Linux `checks` job; the macOS simulator job runs nightly
or by hand (Actions → Rust checks → Run workflow). How it works, on Linux (no
linking): point `SDKROOT` at a directory whose
`usr/include` holds stub `TargetConditionals.h` (TARGET_OS_IPHONE etc.),
`assert.h`, `string.h`; set `CC_aarch64_apple_ios_sim=clang`,
`AR_aarch64_apple_ios_sim=llvm-ar`, `CFLAGS_aarch64_apple_ios_sim=-ffreestanding`;
then `cargo check --target aarch64-apple-ios-sim`. C objects are never linked,
so stubs only need to let `ring` compile.

## Phase 2 — Boot in simulator, screenshot

Goal: map UI renders in iOS simulator; CI uploads screenshot + log.

- `src/main.rs`: on iOS, data directory =
  `$HOME/Library/Application Support/roamlog` (sandbox `HOME`; private app
  data, not shown in Files; no Objective-C needed).
- Seed data: `tests/fixtures/downtown.pmtiles` as basemap plus
  `ios/seed-trips.jsonl` (10 walks inside fixture bounds) replayed through the CLI.
- `cargo xtask ios [--sim] [--release]`: build binary, assemble
  `Roamlog.app` (binary, `Info.plist`, icons, assets). Bundle ID `dev.traveler`
  (matches Android package). Minimum iOS 17 (`CLBackgroundActivitySession`).
- CI steps:
  1. `xcrun simctl boot` a current iPhone device type.
  2. `xcrun simctl install booted Roamlog.app`.
  3. Seed data: copy demo journal/basemap into
     `xcrun simctl get_app_container booted dev.traveler data`.
  4. `xcrun simctl launch --console-pty booted dev.traveler` (env via
     `SIMCTL_CHILD_*`), wait, `xcrun simctl io booted screenshot`.
  5. Upload screenshot, stdout/stderr, crash logs as artifacts.
- Expected fixes: winit iOS lifecycle vs zgui run loop, safe-area insets
  (content under notch/home bar), surface resize/scale factor, Metal limits on
  simulator (texture formats, feature flags in map/terrain pipelines).

Exit: screenshot shows map with demo trips.

Findings (2026-11-19), all fixed:
- winit on iOS sizes the UIWindow from the requested inner size; Roamlog no
  longer sets its 460x900 desktop size on iOS.
- winit's `request_redraw` on iOS is `setNeedsDisplay`, which UIKit drops
  when called inside `drawRect:` (where every zgui frame runs), so the UI froze
  after its first frame. zgui `ios` branch delivers iOS redraws through the
  loop's own redraw event.
- winit's inner size on iOS is only the safe area; zgui sizes the surface to
  the outer (full-screen) size and sends the safe area as insets, which
  Roamlog's CSS already uses.
- Roamlog's theme does not follow system appearance, so CI screenshots once.
- `ZGUI_LATENCY=<file>` (set via `SIMCTL_CHILD_ZGUI_LATENCY`, path inside app
  container `tmp/`) records every loop wake, event and park; it found the
  redraw bug.

## Phase 3 — Scripted interaction in CI

Goal: touch input works; repeatable UI scenarios.

- Install `idb` (`idb-companion` via Homebrew + `fb-idb` via pip) on runner;
  drive taps/swipes with `idb ui tap x y` / `idb ui swipe`.
- Scenarios: open History, select trip, pan/zoom map, open Settings; screenshot
  each step.
- Keep scenario script in repo (`xtask ios-scenario` or shell script) so it
  runs same way each time.

Exit: scenario screenshots match expectations; touch, scroll, sheets work.

Done (2026-11-19). `ios/scenario.sh` taps zoom, pans map, opens and closes the
category picker, opens History, scrolls 10 seeded trips, opens Settings and
returns to Map; a step leaving the screen unchanged fails the job. Findings:
- `idb_companion` takes ~25 s to listen on CI; CI retries `idb connect`.
- `idb list-targets` times out describing the simulator and makes the client
  forget the companion; scenario passes `--udid` and never lists targets.
- `launchctl list | grep -q` SIGPIPEs under `pipefail`; use `grep > /dev/null`.
- Vello needs `INDIRECT_EXECUTION`, absent on the simulator's Metal; opening a
  sheet panicked. zgui `ios` branch falls back to coverage renderer.

## Phase 4 — iOS services

Decision (2026-11-19): services live in Roamlog `src/ios/` (objc2 0.6 crates),
not zgui. Each zgui change needs a fork push and rev bump per iteration;
Roamlog-side code checks locally with stub SDK. Move to zgui once stable.
Done: CoreLocation watch (`src/ios/location.rs`); document open/export, share
sheet, Settings URL (`src/ios/files.rs`); local notifications
(`src/ios/notify.rs`); facade `src/ios/mod.rs` reached through
`crate::platform`. Info.plist: `NSLocationWhenInUseUsageDescription`,
`UIBackgroundModes: location`, `roamlog` URL scheme, icon files. Keyboard text
entry works through winit (`set_ime_allowed` → `becomeFirstResponder`).
Verified by `ios/scenario.sh`: sharing set to Always (mode saved, notification
prompt accepted), pairing screen and invite code shown, `roamlog://` link
received; trip recorded from a `simctl location` route
(moving fixes and finish in the journal), GPX export file written and export
picker shown and dismissed, trip renamed by typing on the on-screen keyboard.
Findings:
- The redraw fix had no display pacing: once something animated continuously,
  frames ran back to back and UIKit never dispatched queued touches. zgui
  `ios` branch now re-issues each redraw as `setNeedsDisplay` from the loop's
  own turn, so UIKit paces frames.
- winit installs no `UIApplication` delegate, so Roamlog sets its own after
  launch for `application:openURL:options:`; `roamlog://pair/` links opened
  from outside (verified with `simctl openurl`) fill the pairing form. It was
  briefly blamed for the touch starvation above and removed; it was innocent.

Needed (Roamlog uses these):
- location: watches via CLLocationManager; background updates
  (`allowsBackgroundLocationUpdates`, `CLBackgroundActivitySession`).
- permission: location When In Use → Always; notifications.
- notification: local notifications (UNUserNotificationCenter); no ongoing
  foreground-service notification on iOS.
- document: open (UIDocumentPickerViewController) and save/export.
- share: UIActivityViewController for text/files.
- insets + keyboard: safe area, keyboard frame.
- system: status bar style (light/dark), open app settings URL.
- intent: incoming URL (custom scheme for invite links,
  `CFBundleURLTypes`).

Skip (Roamlog does not use): bluetooth, nfc, usb, media, power.

Simulator tests:
- `xcrun simctl privacy booted grant location dev.traveler`.
- `xcrun simctl location booted start <waypoints>` feeds moving GPS.
- `xcrun simctl openurl booted <invite link>` tests URL handling.

Exit: location updates reach app; notification posts; document pick/export
round-trip works in simulator.

## Phase 5 — Roamlog iOS module

- `src/ios.rs`: same facade functions as `android.rs` (`record`, `backup`,
  `restore`, `export_gpx`, `import_basemap`, `share_text`, `scan_invite`,
  `pick_avatar`, `open_settings`, sharing start/stop), calling shared policy
  from Phase 0.
- `platform` module re-exporting `android` or `ios`; switch `crate::android::`
  call sites to it. No trait.
- Gates meaning "mobile" (friends UI, place alerts, identity, friend history
  sources) become `any(target_os = "android", target_os = "ios")`.
- Route direct zgui Android calls through facade: notifications
  (`sharing/node.rs`), system bar (`ui/mod.rs`), battery.
- Recording in app process (no `:recorder` process on iOS); journal `flock`
  still fine; recovery covers app killed mid-trip.
- Updater (`updates.rs`, update UI) off on iOS.
- Enable `sharing` feature: iroh/tokio on iOS; identity key in Keychain
  (replaces Android Keystore `SharingIdentity.java`).
- Sharing lifecycle: iOS keeps app alive only while location updates run, so
  Always mode = continuous background location; Off/During trips = no incoming
  friend data while suspended. Document this in user guide.

Simulator tests: record trip from `simctl location` route, finish, see in
History; backup/restore; GPX export; two simulators on same runner pair via
invite link and exchange locations (stretch).

Exit: full feature parity in simulator, minus device-only items below.

Done (2026-11-19). `sharing/android_app.rs` became `sharing/app.rs`, shared by
both platforms; Keystore, DNS context, locate service and battery stay
Android-only. iOS keeps the iroh key in the Keychain (`sharing/keychain.rs`,
`security-framework`), runs the node on the journal worker, keeps a coarse
(10 m / 20 m filter) background location watch while sharing Always, feeds
recording fixes to friends, and posts local notifications. iOS builds require
`--features sharing`, like Android. Findings:
- `libc` 0.2.190+ gates Darwin route constants and `in6_ifreq` to macOS;
  iroh's `netwatch` and `netdev` stopped building for iOS. Patched copies in
  `vendor/` (see NOTICE); `libc` cannot be pinned back because `mio` needs it.
- Simulator apps are ad-hoc signed without entitlements, so Keychain failed
  ("A required entitlement isn't present"). Simulator builds embed
  `ios/simulator.entitlements` in `__TEXT,__entitlements`; device builds get
  entitlements from SideStore's re-signing.
- Device builds must set `IPHONEOS_DEPLOYMENT_TARGET` for Rust and C alike, or
  linking fails on `___chkstk_darwin`.

## Phase 6 — Distribution

- CI builds device IPA: `aarch64-apple-ios` release, `Payload/Roamlog.app`
  zipped to `roamlog.ipa`, unsigned. Attach to existing nightly release next to
  APKs.
- Publish SideStore/AltStore source JSON (app name, bundle ID, version, IPA
  URL, sha256) as release asset or in repo; users add source once, get updates.
- README: install via SideStore, 7-day refresh, 3-app limit on free Apple ID.

Done (2026-11-19). `cargo xtask ios --release --device` writes
`target/aarch64-apple-ios/release/roamlog.ipa` (unsigned, icon from
`assets/logo.png`). On master, `ios-release` builds it and the release job
attaches it to the nightly release, then `ios/source.py` rewrites
`roamlog-source.json` on the fixed `sidestore` release, the URL users add in
SideStore. Version is `0.1.<code>` / build `<code>`, same code as the APK, so
every nightly is an update. The per-push `ios` job builds and checks the same
IPA layout. Release publishing itself runs only on master, so it is first
exercised by the merge.

## Phase 7 — Real device validation (needs iPhone)

Cannot be done in simulator. Mirrors Android validation in
[location sharing plan](location-sharing-plan.md):
- background recording with screen off, long drive;
- iOS kills app mid-trip → recovery on relaunch;
- battery: 2 h driving, 8 h stationary with Always sharing;
- Wi-Fi↔LTE, airplane-mode recovery, two-way sharing with Android phone;
- Keychain identity survives SideStore re-sign every 7 days;
- debugging from Linux via `idevice` (syslog, AFC pull of journal, simulated
  location).

Status: blocked on hardware (no iPhone) for the device-only items.

Simulator stand-ins, run by `ios/scenario.sh` on every push (2026-11-19, run
44533493745):
- background recording: Home pressed mid-trip; 24 fixes recorded in 28 s
  behind the home screen while `simctl location` kept moving;
- app ended mid-trip: `simctl terminate` during a second trip, reopen; trip
  paused exactly at its last fix, UI offers Resume/Finish.
These prove the code paths, not device behaviour: iOS may suspend or end a
real app differently (low power, memory pressure, long screen-off).

Still needs a real iPhone: long screen-off drive, battery (2 h driving, 8 h
Always), Wi-Fi↔LTE and airplane-mode recovery, two-way sharing with an
Android phone, Keychain identity across SideStore's 7-day re-sign.

How to run it from Linux, once a phone exists (commands from libimobiledevice;
the Rust `idevice` crate offers the same services; untested here):
1. Install SideStore on the phone, add
   `https://github.com/Quick-Flash/Roamlog/releases/download/sidestore/roamlog-source.json`,
   install Roamlog. Enable Developer Mode for location simulation.
2. Logs: `idevicesyslog | grep -i roamlog` while testing; Rust panics and
   `places::log` lines appear there.
3. Crashes: `idevicecrashreport -e out/` then look for `Roamlog-*.ips`.
4. Data: SideStore signs with a development certificate, so the app container
   should be vended over house_arrest (`ifuse --container dev.traveler mnt/`);
   journal is `Library/Application Support/roamlog/trips.jsonl`.
5. Simulated GPS on device: `idevicesetlocation -- 45.5158 -122.6788` (needs
   the developer disk image mounted).
6. Run the checklist above; record results here as Android's were in the
   [location sharing plan](location-sharing-plan.md).

## Risks

- zgui on iOS may need deeper changes than estimated (event loop ownership,
  text input/IME, accessibility). Phases 1–2 surface this early and cheaply.
- No device until Phase 7: simulator-only work can hide real-device bugs.
  Buying used iPhone before Phase 5 strongly recommended.
- SideStore re-sign may change Keychain access group → identity loss. Verify
  in Phase 7; fallback is file-stored key protected by Data Protection.
- CI loop is slow (minutes per run). Keep jobs narrow, cache `target/`.

## Known gaps (2026-11-19)

Closed since first listed: keyboard height reaches zgui as an inset (zgui
`ios` branch observes `UIKeyboardWillChangeFrameNotification`; editor moves
above the keyboard in the scenario); status bar follows Roamlog's own theme
(`zgui::app::ios::status_bar_dark`, dark text verified in light theme);
battery level is shared; a friend's refresh request starts 20 s of precise
fixes; notifications also show while Roamlog is in front
(`UNUserNotificationCenterDelegate`). The last three are compiled and wired but
not exercised in the simulator: they need a paired friend.

Open:
- No VoiceOver: accesskit has no iOS adapter.
- Everything is simulator-verified only until Phase 7 runs on a device.
