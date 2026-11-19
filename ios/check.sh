#!/usr/bin/env bash
# Type-checks the iOS app on Linux, no Mac or Apple SDK needed. C build scripts (ring) only
# need a few headers to compile objects that are never linked, so a stub SDK stands in.
# Catches iOS compile errors; linking and running need macOS (ios/package.sh).
# Usage: ios/check.sh [extra cargo check args]
set -euo pipefail
cd "$(dirname "$0")/.."
sdk="$(mktemp -d)"
trap 'rm -rf "$sdk"' EXIT
mkdir -p "$sdk/usr/include"
cat > "$sdk/usr/include/TargetConditionals.h" <<'EOF'
#define TARGET_OS_MAC 1
#define TARGET_OS_OSX 0
#define TARGET_OS_IPHONE 1
#define TARGET_OS_IOS 1
#define TARGET_CPU_ARM64 1
EOF
cat > "$sdk/usr/include/assert.h" <<'EOF'
#define assert(x) ((void)0)
#define static_assert _Static_assert
EOF
cat > "$sdk/usr/include/string.h" <<'EOF'
#include <stddef.h>
void *memcpy(void *, const void *, size_t); void *memmove(void *, const void *, size_t);
void *memset(void *, int, size_t); int memcmp(const void *, const void *, size_t);
size_t strlen(const char *);
EOF
ar="$(command -v llvm-ar || ls /usr/bin/llvm-ar-* /usr/lib/llvm-*/bin/llvm-ar 2> /dev/null | sort -V | tail -n 1)"
[[ -n "$ar" ]] || { echo 'ios/check.sh needs clang and llvm-ar' >&2; exit 1; }
export SDKROOT="$sdk"
for target in aarch64_apple_ios aarch64_apple_ios_sim; do
    export "CC_$target=clang" "AR_$target=$ar" "CFLAGS_$target=-ffreestanding"
done
for target in aarch64-apple-ios-sim aarch64-apple-ios; do
    cargo check --locked --package roamlog --features sharing --target "$target" "$@"
done
