#!/usr/bin/env bash
# Builds unsigned Roamlog.app for iOS simulator (default) or device. Needs macOS + Xcode.
# Usage: ios/package.sh [debug|release] [sim|device]
set -euo pipefail
cd "$(dirname "$0")/.."
profile="${1:-debug}"
platform="${2:-sim}"
case "$profile" in debug|release) ;; *) echo 'profile must be debug or release' >&2; exit 2 ;; esac
case "$platform" in
    sim) triple=aarch64-apple-ios-sim; sdk=iPhoneSimulator ;;
    device) triple=aarch64-apple-ios; sdk=iPhoneOS ;;
    *) echo 'platform must be sim or device' >&2; exit 2 ;;
esac
min_ios=17.0
export IPHONEOS_DEPLOYMENT_TARGET="$min_ios"
args=()
if [[ "$profile" == release ]]; then args+=(--release); fi
# Simulator apps are only ad-hoc signed; the simulator reads entitlements from this binary
# section instead. Keychain (sharing identity) needs an access group.
if [[ "$platform" == sim ]]; then
    entitlements="$(pwd)/ios/simulator.entitlements"
    export CARGO_TARGET_AARCH64_APPLE_IOS_SIM_RUSTFLAGS="-C link-arg=-Wl,-sectcreate,__TEXT,__entitlements,$entitlements"
fi
# macOS bash 3.2 treats empty "${args[@]}" as unbound under set -u.
cargo build --locked --package roamlog --bin roamlog --features sharing --target "$triple" ${args[@]+"${args[@]}"}

target_dir="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
app="$target_dir/$triple/$profile/Roamlog.app"
rm -rf "$app"
mkdir -p "$app"
cp "$target_dir/$triple/$profile/roamlog" "$app/Roamlog"
version_code="${ROAMLOG_VERSION_CODE:-$((1000 + $(git rev-list --count HEAD 2>/dev/null || echo 0)))}"
# Three integers, increasing every build, so SideStore offers each nightly as an update.
version_name="${ROAMLOG_VERSION_NAME:-0.1.$version_code}"
# Empty UILaunchScreen makes iOS run app full screen instead of letterboxed.
# Background location mode keeps a recording trip alive with the screen off.
cat > "$app/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>dev.traveler</string>
    <key>CFBundleExecutable</key><string>Roamlog</string>
    <key>CFBundleName</key><string>Roamlog</string>
    <key>CFBundleDisplayName</key><string>Roamlog</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version_name</string>
    <key>CFBundleVersion</key><string>$version_code</string>
    <key>CFBundleSupportedPlatforms</key><array><string>$sdk</string></array>
    <key>MinimumOSVersion</key><string>$min_ios</string>
    <key>LSRequiresIPhoneOS</key><true/>
    <key>UIDeviceFamily</key><array><integer>1</integer></array>
    <key>UIRequiredDeviceCapabilities</key><array><string>arm64</string></array>
    <key>UILaunchScreen</key><dict/>
    <key>CFBundleIcons</key><dict>
        <key>CFBundlePrimaryIcon</key><dict>
            <key>CFBundleIconFiles</key><array><string>AppIcon60x60</string></array>
        </dict>
    </dict>
    <key>UISupportedInterfaceOrientations</key><array><string>UIInterfaceOrientationPortrait</string></array>
    <key>NSLocationWhenInUseUsageDescription</key><string>Roamlog records your trips and shows where you are on the map.</string>
    <key>UIBackgroundModes</key><array><string>location</string></array>
    <key>CFBundleURLTypes</key><array><dict>
        <key>CFBundleURLName</key><string>dev.traveler.pair</string>
        <key>CFBundleURLSchemes</key><array><string>roamlog</string></array>
    </dict></array>
</dict>
</plist>
PLIST
# Home-screen icon as loose files: no asset catalog, so no actool.
sips -z 120 120 assets/logo.png --out "$app/AppIcon60x60@2x.png" > /dev/null
sips -z 180 180 assets/logo.png --out "$app/AppIcon60x60@3x.png" > /dev/null
# Simulator needs a signature; ad-hoc is enough. Device builds are signed later by SideStore.
if [[ "$platform" == sim ]]; then
    codesign --force --sign - "$app"
    echo "$app"
    exit
fi
# Unsigned IPA for SideStore: the app inside a Payload folder, zipped.
ipa="$target_dir/$triple/$profile/roamlog.ipa"
staging="$(mktemp -d)"
mkdir "$staging/Payload"
cp -R "$app" "$staging/Payload/"
rm -f "$ipa"
(cd "$staging" && zip -qry "$ipa" Payload)
rm -rf "$staging"
echo "$ipa"
