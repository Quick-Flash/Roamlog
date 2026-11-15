#!/usr/bin/env bash
# Adapted from Zgui examples/android/package.sh (Apache-2.0).
# Copyright 2026 Leonard Seibold; modified for Roamlog. See LICENSE-ZGUI.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${ANDROID_HOME:?Set ANDROID_HOME to Android SDK directory}"
: "${ANDROID_NDK_HOME:?Set ANDROID_NDK_HOME to NDK version directory}"
profile="${1:-debug}"
case "$profile" in debug|release) ;; *) echo 'Usage: android/package.sh [debug|release]' >&2; exit 2 ;; esac
android_jar="$ANDROID_HOME/platforms/android-35/android.jar"
build_tools="$ANDROID_HOME/build-tools/35.0.0"
ndk_tools="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
for tool in "$android_jar" "$build_tools/aapt" "$build_tools/zipalign" "$build_tools/apksigner" "$ndk_tools/aarch64-linux-android26-clang"; do
    [[ -f "$tool" ]] || { echo "Missing Android tool: $tool" >&2; exit 1; }
done
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$ndk_tools/aarch64-linux-android26-clang"
export CC_aarch64_linux_android="$CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER"
export AR_aarch64_linux_android="$ndk_tools/llvm-ar"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="${CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS:-} -C link-arg=-Wl,-z,max-page-size=16384"
args=()
if [[ "$profile" == release ]]; then args+=(--release); fi
cargo build --locked --package roamlog --lib --target aarch64-linux-android "${args[@]}"
target_dir="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
# Full history makes commit count monotonic on ordinary master pushes.
export ROAMLOG_VERSION_CODE="${ROAMLOG_VERSION_CODE:-$(python3 - <<'PYCODE'
import subprocess
try:
    print(1000 + int(subprocess.check_output(['git', 'rev-list', '--count', 'HEAD'], text=True)))
except (OSError, subprocess.CalledProcessError):
    print(1)
PYCODE
)}"
export ROAMLOG_VERSION_NAME="${ROAMLOG_VERSION_NAME:-0.1.0-dev.$ROAMLOG_VERSION_CODE}"
manifest="$work/AndroidManifest.xml"
python3 - "$manifest" <<'PYCODE'
import os, pathlib, re
code = int(os.environ['ROAMLOG_VERSION_CODE'])
name = os.environ['ROAMLOG_VERSION_NAME']
if not 1 <= code <= 2100000000 or not re.fullmatch(r'[a-zA-Z0-9._-]+', name):
    raise SystemExit('Invalid Android version code/name')
s = pathlib.Path('android/AndroidManifest.xml').read_text()
s = re.sub(r'android:versionCode="[^"]+"', f'android:versionCode="{code}"', s, count=1)
s = re.sub(r'android:versionName="[^"]+"', f'android:versionName="{name}"', s, count=1)
pathlib.Path(os.sys.argv[1]).write_text(s)
PYCODE
name=roamlog
# ROAMLOG_DEMO=1: separate debuggable "Roamlog Demo" app (dev.traveler.demo) with its own
# storage, for demo videos with made-up data. Real app and its trips stay untouched.
if [[ "${ROAMLOG_DEMO:-0}" == 1 ]]; then
    ROAMLOG_DEBUGGABLE=1
    name=roamlog-demo
fi
if [[ "${ROAMLOG_DEBUGGABLE:-0}" == 1 ]]; then
    manifest="$work/AndroidManifest.xml"
    python3 - "$manifest" "${ROAMLOG_DEMO:-0}" <<'PY'
import pathlib, sys
source = pathlib.Path(sys.argv[1]).read_text()
source = source.replace('<application ', '<application android:debuggable="true" ', 1)
if sys.argv[2] == '1':
    source = source.replace('dev.traveler.zgui.files', 'dev.traveler.demo.zgui.files').replace('package="dev.traveler"', 'package="dev.traveler.demo"', 1)
    source = source.replace('android:label="Roamlog"', 'android:label="Roamlog Demo"', 1)
    # Simulated GPS: recorder is not a location service, so no location permission needed.
    source = source.replace('android:foregroundServiceType="location"', 'android:foregroundServiceType="specialUse"', 1)
    source = source.replace('<uses-permission android:name="android.permission.FOREGROUND_SERVICE_LOCATION" />', '<uses-permission android:name="android.permission.FOREGROUND_SERVICE_SPECIAL_USE" />', 1)
pathlib.Path(sys.argv[1]).write_text(source)
PY
fi
mkdir -p "$work/lib/arm64-v8a" "$work/res/drawable-nodpi"
cp -R android/res/. "$work/res/"
# Package original artwork unchanged; adaptive drawable supplies mask-safe padding.
cp assets/logo.png "$work/res/drawable-nodpi/roamlog_logo.png"
# Keep prebuilt Zgui host; merge only APK verification/installer glue.
zgui_dex="$(cargo metadata --format-version 1 | python3 -c 'import json,os,sys; m=json.load(sys.stdin); print(next(os.path.join(os.path.dirname(p["manifest_path"]), "android", "zgui.dex") for p in m["packages"] if p["name"] == "zgui-platform-winit"))')"
mkdir -p "$work/java" "$work/dex"
javac --release 11 -cp "$android_jar" -d "$work/java" android/src/dev/roamlog/android/Updates.java
jar cf "$work/updates.jar" -C "$work/java" .
"$build_tools/d8" --lib "$android_jar" --min-api 26 --output "$work/dex" "$zgui_dex" "$work/updates.jar"
cp "$work/dex/classes.dex" "$work/classes.dex"
cp "$target_dir/aarch64-linux-android/$profile/libroamlog.so" "$work/lib/arm64-v8a/"
"$ndk_tools/llvm-strip" "$work/lib/arm64-v8a/libroamlog.so"
"$build_tools/aapt" package -f -M "$manifest" -I "$android_jar" -S "$work/res" -F "$work/roamlog.apk"
python3 - "$work" <<'PY'
import pathlib, sys, zipfile
root = pathlib.Path(sys.argv[1])
with zipfile.ZipFile(root / 'roamlog.apk', 'a') as apk:
    apk.write(root / 'classes.dex', 'classes.dex', compress_type=zipfile.ZIP_DEFLATED)
    apk.write(root / 'lib/arm64-v8a/libroamlog.so', 'lib/arm64-v8a/libroamlog.so', compress_type=zipfile.ZIP_STORED)
PY
mkdir -p "$target_dir/$profile/apk"
output="$target_dir/$profile/apk/$name.apk"
"$build_tools/zipalign" -f -P 16 4 "$work/roamlog.apk" "$output"
keystore="${ROAMLOG_KEYSTORE:-$PWD/.private/signing/roamlog.keystore}"
# Preserve development installs created before durable signing storage.
if [[ -z "${ROAMLOG_KEYSTORE:-}" && ! -f "$keystore" && -f "$target_dir/android/debug.keystore" ]]; then
    mkdir -p "$(dirname "$keystore")"
    cp "$target_dir/android/debug.keystore" "$keystore"
    chmod 600 "$keystore"
fi
export ROAMLOG_KEYSTORE_PASSWORD="${ROAMLOG_KEYSTORE_PASSWORD:-android}"
if [[ ! -f "$keystore" ]]; then
    if [[ -n "${ROAMLOG_KEYSTORE:-}" ]]; then
        echo 'Configured signing key missing; refusing to generate replacement' >&2
        exit 1
    fi
    mkdir -p "$(dirname "$keystore")"
    (umask 077; keytool -genkeypair -keystore "$keystore" -storepass:env ROAMLOG_KEYSTORE_PASSWORD -keypass:env ROAMLOG_KEYSTORE_PASSWORD -alias androiddebugkey -keyalg RSA -keysize 2048 -validity 10000 -dname 'CN=Android Debug,O=Android,C=US')
fi
"$build_tools/apksigner" sign --ks "$keystore" --ks-pass env:ROAMLOG_KEYSTORE_PASSWORD "$output"
"$build_tools/apksigner" verify "$output"
echo "$output"
