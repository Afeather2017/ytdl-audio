#!/bin/bash
# Build and sign ytdl-demo Android release packages.

set -euo pipefail

ANDROID_HOME="${ANDROID_HOME:-$HOME/.local/android}"
BUILD_TOOLS_VERSION="${BUILD_TOOLS_VERSION:-35.0.0}"
BUILD_TOOLS="$ANDROID_HOME/build-tools/$BUILD_TOOLS_VERSION"
KEYSTORE="${KEYSTORE:-${ANDROID_HOME}/release.keystore}"
KEY_ALIAS="${KEY_ALIAS:-release}"
KEY_STOREPASS="${KEY_STOREPASS:-123456}"
KEY_KEYPASS="${KEY_KEYPASS:-$KEY_STOREPASS}"
CI_MODE="${CI:-false}"
INSTALL_ON_DEVICE="${INSTALL_ON_DEVICE:-true}"
ANDROID_TARGET="${ANDROID_TARGET:-aarch64}"

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ANDROID_DIR="$PROJECT_ROOT/src-tauri/gen/android"
APK_GLOB="$ANDROID_DIR/app/build/outputs/apk"
AAB_GLOB="$ANDROID_DIR/app/build/outputs/bundle"

echo "=== ytdl-demo Android Build Script ==="
echo "Project root: $PROJECT_ROOT"
echo "Android build tools: $BUILD_TOOLS"
echo "CI mode: $CI_MODE"
echo "Android target: $ANDROID_TARGET"
echo

if [ "$CI_MODE" = "true" ] && [ -z "${ANDROID_KEY_BASE64:-}" ]; then
    echo "ANDROID_KEY_BASE64 is required for CI release builds." >&2
    exit 1
fi

if [ -n "${ANDROID_KEY_BASE64:-}" ]; then
    echo "[1/5] Restoring CI keystore..."
    KEYSTORE="${RUNNER_TEMP:-/tmp}/ytdl-demo-upload-keystore.jks"
    printf '%s' "$ANDROID_KEY_BASE64" | base64 --decode > "$KEYSTORE"
    KEY_ALIAS="${ANDROID_KEY_ALIAS:?ANDROID_KEY_ALIAS is required when ANDROID_KEY_BASE64 is set}"
    KEY_STOREPASS="${ANDROID_KEY_PASSWORD:?ANDROID_KEY_PASSWORD is required when ANDROID_KEY_BASE64 is set}"
    KEY_KEYPASS="${ANDROID_KEY_PASSWORD}"
elif [ ! -f "$KEYSTORE" ]; then
    echo "[1/5] Generating local keystore..."
    keytool -genkey -v -keystore "$KEYSTORE" -alias "$KEY_ALIAS" \
        -keyalg RSA -keysize 2048 -validity 10000 \
        -storepass "$KEY_STOREPASS" -keypass "$KEY_KEYPASS" \
        -dname "CN=ytdl-demo,O=ytdl-demo,C=US"
else
    echo "[1/5] Using existing keystore: $KEYSTORE"
fi

echo
echo "[2/5] Building Android packages..."
cd "$PROJECT_ROOT"
npx tauri android build --apk --aab --target "$ANDROID_TARGET" "$@"

echo
echo "[3/5] Signing APK..."
UNSIGNED_APK="$(find "$APK_GLOB" -type f -name '*unsigned.apk' | sort | tail -n 1 || true)"
if [ -z "$UNSIGNED_APK" ]; then
    echo "No unsigned APK found under $APK_GLOB" >&2
    exit 1
fi

SIGNED_APK="${UNSIGNED_APK%unsigned.apk}signed.apk"
ALIGNED_APK="${UNSIGNED_APK%unsigned.apk}aligned.apk"
"$BUILD_TOOLS/zipalign" -f 4 "$UNSIGNED_APK" "$ALIGNED_APK"
"$BUILD_TOOLS/apksigner" sign \
    --ks "$KEYSTORE" \
    --ks-key-alias "$KEY_ALIAS" \
    --ks-pass "pass:$KEY_STOREPASS" \
    --key-pass "pass:$KEY_KEYPASS" \
    --out "$SIGNED_APK" \
    "$ALIGNED_APK"

echo
echo "[4/5] Signing AAB..."
UNSIGNED_AAB="$(find "$AAB_GLOB" -type f -name '*.aab' ! -name '*-signed*' | sort | tail -n 1 || true)"
SIGNED_AAB=""
if [ -n "$UNSIGNED_AAB" ]; then
    SIGNED_AAB="${UNSIGNED_AAB%.aab}-signed.aab"
    jarsigner \
        -keystore "$KEYSTORE" \
        -storepass "$KEY_STOREPASS" \
        -keypass "$KEY_KEYPASS" \
        -signedjar "$SIGNED_AAB" \
        "$UNSIGNED_AAB" \
        "$KEY_ALIAS"
    jarsigner -verify "$SIGNED_AAB" >/dev/null
fi

echo
echo "[5/5] Collecting build outputs..."
find "$APK_GLOB" -type f \( -name '*.apk' -o -name '*mapping.txt' \) | sort || true
find "$AAB_GLOB" -type f -name '*.aab' | sort || true

echo
echo "Primary signed APK: $SIGNED_APK"
if [ -n "$SIGNED_AAB" ]; then
    echo "Primary signed AAB: $SIGNED_AAB"
fi

if [ "$CI_MODE" = "true" ] || [ "$INSTALL_ON_DEVICE" != "true" ]; then
    echo "Skipping device install."
    exit 0
fi

if command -v adb >/dev/null 2>&1; then
    if adb install -r "$SIGNED_APK"; then
        echo "Installation complete!"
    else
        echo "adb install failed or no device found; skipping."
    fi
else
    echo "adb not found; skipping install."
fi
