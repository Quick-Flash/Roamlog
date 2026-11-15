# Android networking probe

Standalone arm64 Android app for testing Iroh with Keystore-backed identities.
Package `dev.roamlog.sharingprobe` has separate storage and Keystore namespace
from Roamlog and from identity instrumentation tests. No GPS permissions, trip
data, or background service. Phase 2 adds [pairing and friend records](sharing-pairing.md).
Native library builds with
`--no-default-features --features sharing`; Zgui UI and recorder are excluded.

## Build and install

Requires Android SDK platform/build-tools 35, NDK, JDK 17+, and installed Rust
`aarch64-linux-android` target:

```sh
export ANDROID_HOME=/path/to/android-sdk
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/28.2.13676358"
bash android/probe/package.sh
adb install -r target/sharing-probe/sharing-probe.apk
adb shell am start -n dev.roamlog.sharingprobe/dev.roamlog.android.SharingProbe
```

APK uses its own test signing key under `target/sharing-probe/probe.keystore`.
Keep that file to update an installed probe without uninstalling and losing its
identity. No production signing keys are used. Native library and APK alignment
support 16 KB memory pages.

## Two-phone check

1. Install probe on both phones. Tap **Show identity** on each. Restart app and
   repeat to verify peer ID stays unchanged. Compare IDs through trusted channel.
2. On Alice, paste Bob's peer ID into input and tap **Listen**.
3. Wait for Alice's address JSON (relay startup waits at most 30 seconds), then
   tap **Copy address JSON**. Transfer that public address to Bob.
4. On Bob, paste Alice's address JSON and tap **Connect and watch**.
5. Both phones should show authenticated probe successes and selected path:
   `direct`, `relay`, or `unknown`. Client sends one nonce every 10 seconds over
   existing connection. Same bounded retry policy as desktop harness applies.
6. Switch Wi-Fi/cellular while keeping both activities visible. Record recovery
   time and selected-path changes. Successful connection authenticates peer ID;
   no location access is granted.
7. Tap **Stop**, or leave activity. Ordinary probe endpoint closes. Return and
   start explicitly to resume. Locking screen also stops ordinary probe. Bounded
   Phase 2 pairing can briefly continue during invitation handoff; see pairing guide.

Listener rejects unexpected peer IDs and stays available for approved peer.
Client stops on application-close responses (including authorization denial)
and malformed responses. Network loss or probe timeout triggers reconnect;
exhausting six dial attempts ends session. Stop also cancels relay wait, retry
backoff, active streams, and bind startup. Worker finishes current Keystore/file
operation before honoring stop. Starting another session waits for cleanup.

Address JSON and IDs are public connectivity information. UI does not display
secret keys. Native bridge installs Android application context for Iroh DNS,
retains that context for process lifetime, and bounds temporary JNI references
while delivering progress. Network errors use generic UI failure text; failed
identity loads preserve stored identity.

## Validation status

Built/signed APK, checked JNI exports, compiled Android Rust with and without
Zgui UI, and exercised shared probe logic on desktop. Physical-device execution,
Android JNI runtime behavior, Keystore checks, cellular/relay reachability, and
network-switch recovery remain pending. Foreground-service experiment comes
after foreground networking works on devices. This harness cannot validate
screen-off or continuous background operation.
