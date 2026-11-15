#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
: "${ANDROID_HOME:?Set ANDROID_HOME to Android SDK directory}"
: "${ANDROID_NDK_HOME:?Set ANDROID_NDK_HOME to Android NDK directory}"
android_jar="$ANDROID_HOME/platforms/android-35/android.jar"
build_tools="$ANDROID_HOME/build-tools/35.0.0"
ndk_tools="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$ndk_tools/aarch64-linux-android26-clang"
export CC_aarch64_linux_android="$CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER"
export AR_aarch64_linux_android="$ndk_tools/llvm-ar"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="${CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS:-} -C link-arg=-Wl,-z,max-page-size=16384"
cargo build --locked --no-default-features --features sharing --lib --target aarch64-linux-android
target_dir="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
output="$target_dir/sharing-probe"
mkdir -p "$output/java" "$output/dex" "$output/lib/arm64-v8a"
javac --release 11 -Xlint:all -Werror -cp "$android_jar" -d "$output/java" \
    android/src/dev/roamlog/android/SharingIdentity.java android/probe/*.java
jar cf "$output/probe.jar" -C "$output/java" .
"$build_tools/d8" --lib "$android_jar" --min-api 26 --output "$output/dex" "$output/probe.jar"
cp "$target_dir/aarch64-linux-android/debug/libroamlog.so" "$output/lib/arm64-v8a/"
"$ndk_tools/llvm-strip" "$output/lib/arm64-v8a/libroamlog.so"
"$build_tools/aapt" package -f -M android/probe/AndroidManifest.xml -I "$android_jar" -F "$output/unsigned.apk"
python3 - "$output" <<'PY'
import pathlib, sys, zipfile
root = pathlib.Path(sys.argv[1])
with zipfile.ZipFile(root / 'unsigned.apk', 'a') as apk:
    apk.write(root / 'dex/classes.dex', 'classes.dex', compress_type=zipfile.ZIP_DEFLATED)
    apk.write(root / 'lib/arm64-v8a/libroamlog.so', 'lib/arm64-v8a/libroamlog.so', compress_type=zipfile.ZIP_STORED)
PY
"$build_tools/zipalign" -f -P 16 4 "$output/unsigned.apk" "$output/sharing-probe.apk"
if [[ ! -f "$output/probe.keystore" ]]; then
    keytool -genkeypair -keystore "$output/probe.keystore" -storepass android -keypass android \
        -alias probe -keyalg RSA -keysize 2048 -validity 3650 -dname 'CN=Roamlog Sharing Probe'
fi
"$build_tools/apksigner" sign --ks "$output/probe.keystore" --ks-pass pass:android "$output/sharing-probe.apk"
"$build_tools/apksigner" verify "$output/sharing-probe.apk"
echo "$output/sharing-probe.apk"
