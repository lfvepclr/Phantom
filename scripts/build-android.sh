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

# Oldest NDK whose clang we can drive. r26 is the first with the LLVM-only
# toolchain layout this script relies on (`${target}${api}-clang`).
NDK_MIN_MAJOR=26

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
SO_NAME="libphantom_android.so"
TARGET="aarch64-linux-android"

# The cdylib is `phantom-android` (client/android/rust), not `phantom-client`:
# the JNI surface plus the embedded-server bridge live there so that the client
# core never has to depend on the server crate. See client/android/README.md.
CRATE="phantom-android"

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

# Fail here rather than in the middle of a cargo build: an older NDK does ship
# a clang, just not the one this script names, and the resulting linker error
# gives no hint about the real cause.
NDK_VERSION="$(basename "$ANDROID_NDK_HOME")"
NDK_MAJOR="${NDK_VERSION%%.*}"
if [[ "$NDK_MAJOR" =~ ^[0-9]+$ ]] && (( NDK_MAJOR < NDK_MIN_MAJOR )); then
  echo "ERROR: NDK $NDK_VERSION is older than r${NDK_MIN_MAJOR}." >&2
  echo "Install a supported NDK:" >&2
  echo '  $ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager "ndk;26.1.10909125"' >&2
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
echo "[2/4] cargo build -p $CRATE --lib --target $TARGET ..."
(
  cd "$ROOT"
  export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$LINKER"
  export CC_aarch64_linux_android="$LINKER"
  export CXX_aarch64_linux_android="$CXX"
  export AR_aarch64_linux_android="$AR"
  cargo build $PROFILE_FLAG -p "$CRATE" --lib --target "$TARGET"
)

# Step 3: copy .so into jniLibs.
echo "[3/4] Copying $SO_NAME to $JNI_LIBS_DIR ..."
mkdir -p "$JNI_LIBS_DIR"
cp "$ROOT/target/$TARGET/$CARGO_TARGET_SUBDIR/$SO_NAME" "$JNI_LIBS_DIR/$SO_NAME"
# Strip in the NDK's own toolchain: Gradle cannot (it has no matching
# `llvm-strip` for the target) and an unstripped cdylib is several times larger.
if [[ -x "$TOOLCHAIN/llvm-strip" ]]; then
  "$TOOLCHAIN/llvm-strip" --strip-unneeded "$JNI_LIBS_DIR/$SO_NAME" 2>/dev/null || true
fi
ls -l "$JNI_LIBS_DIR/$SO_NAME"

# Step 4: optional Gradle APK build.
echo "[4/4] Gradle assembleDebug (optional) ..."
if [[ -x "$ANDROID_DIR/gradlew" ]]; then
  # Gradle needs a JDK 17+ and the SDK location. Both are known from the NDK
  # path and the usual JDK install spots, so derive them instead of making the
  # caller export three more variables.
  #
  # Order matters: an exported JAVA_HOME is the user's explicit choice, then a
  # real JDK 17, then whatever the OS thinks is current, then the JDK bundled
  # with Android Studio, then the common Linux path.
  if [[ -n "${JAVA_HOME:-}" && ! -x "$JAVA_HOME/bin/java" ]]; then
    echo "WARN: JAVA_HOME=$JAVA_HOME has no bin/java; looking for another JDK." >&2
    unset JAVA_HOME
  fi
  if [[ -z "${JAVA_HOME:-}" ]]; then
    for candidate in \
      "$(/usr/libexec/java_home -v 17 2>/dev/null || true)" \
      "$(/usr/libexec/java_home 2>/dev/null || true)" \
      "/Applications/Android Studio.app/Contents/jbr/Contents/Home" \
      "/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home" \
      "/usr/lib/jvm/java-17-openjdk-amd64"
    do
      if [[ -n "$candidate" && -x "$candidate/bin/java" ]]; then
        export JAVA_HOME="$candidate"
        break
      fi
    done
  fi
  if [[ -z "${JAVA_HOME:-}" ]]; then
    echo "WARN: no JDK 17 found; Gradle may fail. Set JAVA_HOME and re-run." >&2
  else
    echo "  JDK   : $JAVA_HOME"
  fi
  SDK_ROOT="$(cd "$ANDROID_NDK_HOME/../.." && pwd)"
  if [[ -d "$SDK_ROOT/platforms" ]]; then
    # `local.properties` is gitignored: it points at one machine's SDK.
    printf 'sdk.dir=%s\n' "$SDK_ROOT" > "$ANDROID_DIR/local.properties"
  fi
  ( cd "$ANDROID_DIR" && ./gradlew assembleDebug --console=plain )
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
