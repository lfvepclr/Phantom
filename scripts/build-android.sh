#!/usr/bin/env bash
# scripts/build-android.sh — Build Phantom Android cdylib and optional APK.
#
# Mirrors scripts/build-mac.sh: cargo builds the Rust cdylib, copies it into
# the Android project, then optionally runs Gradle to produce an APK.
#
# Usage:
#   scripts/build-android.sh             # default release
#   scripts/build-android.sh --debug     # debug profile

set -euo pipefail

PROFILE_FLAG="--release"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --debug) PROFILE_FLAG=""; shift ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
ROOT="$SCRIPT_DIR/.."
ANDROID_DIR="$ROOT/client/android"
JNI_LIBS_DIR="$ANDROID_DIR/app/src/main/jniLibs/arm64-v8a"
SO_NAME="libphantom_client.so"
TARGET="aarch64-linux-android"

# Determine Rust target subdir.
if [[ "$PROFILE_FLAG" == "--release" ]]; then
  CARGO_TARGET_SUBDIR="release"
else
  CARGO_TARGET_SUBDIR="debug"
fi

# Locate the Android NDK.
if [[ -z "${ANDROID_NDK_HOME:-}" ]]; then
  DEFAULT_NDK_ROOT="$HOME/Library/Android/sdk/ndk"
  if [[ -d "$DEFAULT_NDK_ROOT" ]]; then
    # Pick the newest installed NDK version.
    ANDROID_NDK_HOME="$(ls -1 "$DEFAULT_NDK_ROOT" | sort -V | tail -n 1)"
    ANDROID_NDK_HOME="$DEFAULT_NDK_ROOT/$ANDROID_NDK_HOME"
  fi
fi

if [[ -z "${ANDROID_NDK_HOME:-}" ]] || [[ ! -d "$ANDROID_NDK_HOME" ]]; then
  echo "ERROR: ANDROID_NDK_HOME is not set or does not exist." >&2
  echo "Install the Android NDK and set ANDROID_NDK_HOME, e.g.:" >&2
  echo "  export ANDROID_NDK_HOME=\$HOME/Library/Android/sdk/ndk/26.1.10909125" >&2
  exit 1
fi

HOST_TAG="darwin-x86_64"
# Allow Linux hosts to use the script as well.
if [[ "$(uname -s)" == "Linux" ]]; then
  HOST_TAG="linux-x86_64"
fi

# API level 34 matches our compileSdk/targetSdk.
API_LEVEL=34
TOOLCHAIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$HOST_TAG/bin"
LINKER="$TOOLCHAIN/${TARGET}${API_LEVEL}-clang"
CXX="$TOOLCHAIN/${TARGET}${API_LEVEL}-clang++"
AR="$TOOLCHAIN/llvm-ar"

if [[ ! -x "$LINKER" ]]; then
  echo "ERROR: linker not found: $LINKER" >&2
  exit 1
fi
if [[ ! -x "$CXX" ]]; then
  echo "ERROR: C++ compiler not found: $CXX" >&2
  exit 1
fi
if [[ ! -x "$AR" ]]; then
  echo "ERROR: archiver not found: $AR" >&2
  exit 1
fi

echo "════════════════════════════════════════════════"
echo "  Phantom Android Build"
echo "════════════════════════════════════════════════"
echo "  NDK    : $ANDROID_NDK_HOME"
echo "  Target : $TARGET"
echo "  Profile: $CARGO_TARGET_SUBDIR"
echo "════════════════════════════════════════════════"

# Step 1: install Rust target (idempotent).
echo "[1/4] rustup target add $TARGET ..."
rustup target add "$TARGET"

# Step 2: cargo build Rust cdylib.
# We set CC/CXX/AR so that build scripts (e.g. ring) find the NDK toolchain.
echo "[2/4] cargo build -p phantom-client --lib --target $TARGET ..."
(
  cd "$ROOT"
  export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$LINKER"
  export CC_aarch64_linux_android="$LINKER"
  export CXX_aarch64_linux_android="$CXX"
  export AR_aarch64_linux_android="$AR"
  cargo build $PROFILE_FLAG -p phantom-client --lib --target "$TARGET"
)

# Step 3: copy .so into jniLibs.
echo "[3/4] Copying $SO_NAME to $JNI_LIBS_DIR ..."
mkdir -p "$JNI_LIBS_DIR"
cp "$ROOT/target/$TARGET/$CARGO_TARGET_SUBDIR/$SO_NAME" "$JNI_LIBS_DIR/$SO_NAME"
ls -l "$JNI_LIBS_DIR/$SO_NAME"

# Step 4: optional Gradle APK build.
echo "[4/4] Gradle assembleDebug (optional) ..."
if [[ -x "$ANDROID_DIR/gradlew" ]]; then
  ( cd "$ANDROID_DIR" && ./gradlew assembleDebug )
  APK_PATH="$ANDROID_DIR/app/build/outputs/apk/debug/app-debug.apk"
  echo ""
  echo "════════════════════════════════════════════════"
  echo "  ✅ Android build complete"
  echo "════════════════════════════════════════════════"
  echo "  SO : $JNI_LIBS_DIR/$SO_NAME"
  echo "  APK: $APK_PATH"
  echo ""
  echo "  Install and run:"
  echo "    adb install -r $APK_PATH"
  echo "════════════════════════════════════════════════"
else
  echo ""
  echo "════════════════════════════════════════════════"
  echo "  ✅ Rust cdylib build complete"
  echo "════════════════════════════════════════════════"
  echo "  SO : $JNI_LIBS_DIR/$SO_NAME"
  echo ""
  echo "  Open $ANDROID_DIR in Android Studio to build the APK."
  echo "════════════════════════════════════════════════"
fi
