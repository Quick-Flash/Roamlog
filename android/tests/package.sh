#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
: "${ANDROID_HOME:?Set ANDROID_HOME to Android SDK directory}"
android_jar="$ANDROID_HOME/platforms/android-35/android.jar"
build_tools="$ANDROID_HOME/build-tools/35.0.0"
output="$PWD/target/sharing-identity-test"
mkdir -p "$output/java" "$output/dex"
javac --release 11 -Xlint:all -Werror -cp "$android_jar" -d "$output/java" \
    android/src/dev/roamlog/android/SharingIdentity.java android/tests/SharingIdentityTest.java
jar cf "$output/tests.jar" -C "$output/java" .
"$build_tools/d8" --lib "$android_jar" --min-api 26 --output "$output/dex" "$output/tests.jar"
"$build_tools/aapt" package -f -M android/tests/AndroidManifest.xml -I "$android_jar" -F "$output/unsigned.apk"
python3 - "$output" <<'PY'
import pathlib, sys, zipfile
root = pathlib.Path(sys.argv[1])
with zipfile.ZipFile(root / 'unsigned.apk', 'a') as apk:
    apk.write(root / 'dex/classes.dex', 'classes.dex', compress_type=zipfile.ZIP_DEFLATED)
PY
"$build_tools/zipalign" -f 4 "$output/unsigned.apk" "$output/identity-test.apk"
if [[ ! -f "$output/test.keystore" ]]; then
    keytool -genkeypair -keystore "$output/test.keystore" -storepass android -keypass android \
        -alias test -keyalg RSA -keysize 2048 -validity 3650 -dname 'CN=Roamlog Identity Tests'
fi
"$build_tools/apksigner" sign --ks "$output/test.keystore" --ks-pass pass:android "$output/identity-test.apk"
"$build_tools/apksigner" verify "$output/identity-test.apk"
echo "$output/identity-test.apk"
