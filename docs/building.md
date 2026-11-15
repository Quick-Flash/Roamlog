# Building Roamlog

## Desktop

Developed on Linux with Rust nightly (rustc 1.96.0-nightly). Install Rust with
rustup, a C/C++ linker, pkg-config, Fontconfig development headers, and Vulkan
runtime/driver. On Debian/Ubuntu, development packages include
`build-essential pkg-config libfontconfig1-dev`. No Gradle or sibling checkouts
are required. First build fetches pinned Cargo dependencies.

```bash
cargo xtask                  # Interactive menu; help when stdin is piped
cargo xtask run              # Desktop app
cargo xtask build --release  # Optimized desktop build
cargo xtask test             # Workspace tests
cargo xtask check            # Fast compile check
cargo xtask fmt              # Format all workspace code
cargo xtask verify           # Formatting, core/UI tests, Clippy (same as CI)
```

Headless command-line recorder uses same journal/tracking core:

```bash
cargo xtask test --no-default-features
# Choose unused journal path for each replay.
cargo run --locked --no-default-features -- /tmp/roamlog-example.jsonl examples/town.geojson < examples/trip.jsonl
```

CLI reads JSON events on stdin, then prints state and optional road sections.
Do not run multiple CLI writers against same journal.

## Android

Supported packaging target: ARM64, Android 8/API 26 or newer. Linux host needs
JDK 17+, Python 3, SDK Platform 35, Build-Tools 35.0.0, and NDK
28.2.13676358 (28c). SDK and NDK paths may be installed separately.

```bash
rustup target add aarch64-linux-android
export ANDROID_HOME=/path/to/android-sdk
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/28.2.13676358"
cargo xtask android --release
adb install --no-incremental -r target/release/apk/roamlog.apk
adb shell am start -n dev.traveler/dev.zgui.android.ZguiActivity
```

`cargo xtask android` builds debug APK; add `--release` for optimized APK.
`--demo` selects separate demo app explicitly. Set SDK/NDK environment variables
before running Android tasks; Linux packaging requires Bash. Tasks do not install
SDKs, modify phone, or seed demo data. Build failure stops task with nonzero exit.
Desktop tasks forward Cargo arguments; e.g. `cargo xtask test history` runs matching
tests, and `cargo xtask run -- --help` passes `--help` to app.

Runner uses only Rust standard library. Existing `android/package.sh` remains
available directly; xtask calls it for APK packaging.

Script locates Zgui’s dex through Cargo metadata, packages native library/icon,
aligns APK for 16 KiB pages, signs it, and verifies signature. `debug` also works.

## Signing and updates

Both build profiles use development signing by default. Key lives at
`target/android/debug.keystore`. Preserve key outside disposable build output;
`cargo clean` removes default key. Set `ROAMLOG_KEYSTORE` to retained key and
`ROAMLOG_KEYSTORE_PASSWORD` to its password. Script expects alias
`androiddebugkey`; this workflow is for development APKs, not store publishing.
Never commit signing keys or passwords.

Install with `adb install -r` using same signing key and package ID to retain
local history. Do not uninstall or clear app data to work around signature
mismatches. Use in-app Back up before installation changes.

## Isolated demo

Demo uses `dev.traveler.demo`, separate storage, and simulated location only.
Never point setup commands at main package.

```bash
cargo run --release --locked --example demo_data -- /tmp/roamlog-demo
cargo xtask android --release --demo
adb install -r target/release/apk/roamlog-demo.apk
```

Generator defaults to public Portland map data and synthetic trips. It needs
network access to obtain map data. After first install, open demo once, then
copy generated files into its private storage:

```bash
for f in basemap.pmtiles trips.jsonl demo-route.json; do
  adb push /tmp/roamlog-demo/$f /data/local/tmp/$f
  adb shell "run-as dev.traveler.demo sh -c 'cat /data/local/tmp/$f > files/$f'"
done
```

These setup commands **replace demo history**. Use only for initial setup or
after backing up demo data. Existing demo needs no reseeding for screenshots.
Capture only demo, never personal routes:

```bash
adb shell am start -n dev.traveler.demo/dev.zgui.android.ZguiActivity
adb exec-out screencap -p > demo.png
```

Review captures for private information before adding to documentation.
