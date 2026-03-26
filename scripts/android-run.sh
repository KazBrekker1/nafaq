#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

PACKAGE_NAME="${PACKAGE_NAME:-com.nafaq.app}"
BUILD_TARGET="${BUILD_TARGET:-aarch64}"
APK_PATH="${APK_PATH:-src-tauri/gen/android/app/build/outputs/apk/universal/release/}"
KEYSTORE_PATH="${KEYSTORE_PATH:-$HOME/.android/debug.keystore}"
KEYSTORE_PASS="${KEYSTORE_PASS:-android}"
ALIGNED_APK_PATH="${ALIGNED_APK_PATH:-/tmp/nafaq-release-aligned.apk}"
SIGNED_APK_PATH="${SIGNED_APK_PATH:-/tmp/nafaq-release-signed.apk}"

if [[ ! -f "$KEYSTORE_PATH" ]]; then
  echo "error: missing keystore at $KEYSTORE_PATH" >&2
  exit 1
fi

resolve_build_tool() {
  local tool_name="$1"
  local latest=""

  while IFS= read -r tool_path; do
    latest="$tool_path"
  done < <(find "${ANDROID_HOME:?}/build-tools" -maxdepth 2 -type f -name "$tool_name" | sort -V)

  if [[ -z "$latest" ]]; then
    echo "error: could not find $tool_name under \$ANDROID_HOME/build-tools" >&2
    exit 1
  fi

  printf '%s\n' "$latest"
}

ZIPALIGN_BIN="$(resolve_build_tool zipalign)"
APKSIGNER_BIN="$(resolve_build_tool apksigner)"

# -- Device selection -------------------------------------------------
ADB_SERIAL=""

if [[ -n "${1:-}" ]]; then
  ADB_SERIAL="$1"
else
  devices=()
  while IFS= read -r d; do
    [[ -n "$d" ]] && devices+=("$d")
  done < <(adb devices | awk 'NR>1 && $2=="device" {print $1}')
  if [[ ${#devices[@]} -eq 0 ]]; then
    echo "error: no devices connected" >&2
    exit 1
  elif [[ ${#devices[@]} -gt 1 ]]; then
    echo "Multiple devices found:"
    for i in "${!devices[@]}"; do
      model=$(adb -s "${devices[$i]}" shell getprop ro.product.model 2>/dev/null | tr -d '\r')
      echo "  [$i] ${devices[$i]}  ${model:-unknown}"
    done
    printf "Select device [0-%d]: " $((${#devices[@]} - 1))
    read -r choice
    if ! [[ "$choice" =~ ^[0-9]+$ ]] || [[ "$choice" -ge ${#devices[@]} ]]; then
      echo "error: invalid selection" >&2
      exit 1
    fi
    ADB_SERIAL="${devices[$choice]}"
  else
    ADB_SERIAL="${devices[0]}"
  fi
fi

if [[ -n "$ADB_SERIAL" ]]; then
  adb() { command adb -s "$ADB_SERIAL" "$@"; }
fi

: "${ANDROID_HOME:=$HOME/Library/Android/sdk}"
if [[ -z "${ANDROID_NDK_HOME:-}" ]]; then
  NDK_DIR="$(ls -d "$ANDROID_HOME/ndk/"* 2>/dev/null | sort -V | tail -1)"
  if [[ -z "$NDK_DIR" ]]; then
    echo "error: no NDK found under $ANDROID_HOME/ndk/" >&2
    exit 1
  fi
  export ANDROID_NDK_HOME="$NDK_DIR"
fi

export CMAKE_TOOLCHAIN_FILE="$(cd "$(dirname "$0")" && pwd)/android-cmake-toolchain.cmake"
export CMAKE_POLICY_VERSION_MINIMUM=3.5

echo ":: Building Android release APK..."
bunx tauri android build --target "$BUILD_TARGET" --apk

if [[ -d "$APK_PATH" ]]; then
  APK_PATH="${APK_PATH%/}/"
  APK_PATH="$(ls -t "$APK_PATH"*.apk 2>/dev/null | head -1)"
fi
if [[ -z "$APK_PATH" || ! -f "$APK_PATH" ]]; then
  echo "error: build completed but no APK found" >&2
  exit 1
fi

INSTALL_APK_PATH="$APK_PATH"
if [[ "$APK_PATH" == *"-unsigned.apk" ]]; then
  echo ":: Signing existing release APK..."
  echo "   source:  $APK_PATH"
  echo "   aligned: $ALIGNED_APK_PATH"
  echo "   signed:  $SIGNED_APK_PATH"
  rm -f "$ALIGNED_APK_PATH" "$SIGNED_APK_PATH"
  "$ZIPALIGN_BIN" -f 4 "$APK_PATH" "$ALIGNED_APK_PATH"
  "$APKSIGNER_BIN" sign \
    --ks "$KEYSTORE_PATH" \
    --ks-pass "pass:$KEYSTORE_PASS" \
    --out "$SIGNED_APK_PATH" \
    "$ALIGNED_APK_PATH"
  INSTALL_APK_PATH="$SIGNED_APK_PATH"
fi

echo ":: Installing APK..."
echo "   apk: $INSTALL_APK_PATH"
if ! adb install -r "$INSTALL_APK_PATH" 2>&1; then
  echo ":: Signature mismatch — uninstalling old app and retrying..."
  adb uninstall "$PACKAGE_NAME"
  adb install "$INSTALL_APK_PATH"
fi

echo ":: Launching..."
adb shell monkey -p "$PACKAGE_NAME" -c android.intent.category.LAUNCHER 1

echo ":: Done."
