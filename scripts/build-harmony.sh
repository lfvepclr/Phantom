#!/usr/bin/env bash
set -euo pipefail

# Build the HarmonyOS NEXT client:
# 1. Compile the Rust core as a NAPI .so for HarmonyOS targets.
# 2. Copy the resulting library into the DevEco Studio entry module.
#
# Requirements:
#   - DevEco Studio NEXT with HarmonyOS SDK
#   - ohos-rs / cargo-ohos or a Rust toolchain configured for
#     aarch64-unknown-linux-ohos

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HARMONY_DIR="${PROJECT_ROOT}/client/harmony"
RUST_DIR="${HARMONY_DIR}/rust"
TARGET="${TARGET:-aarch64-unknown-linux-ohos}"
BUILD_MODE="${BUILD_MODE:-release}"

cd "$RUST_DIR"

# The OHOS clang lives inside the DevEco Studio SDK, and its path differs
# between the IDE install and the standalone command-line tools. `.cargo/config.toml`
# carries the classic IDE path; override it here so any layout works.
if [[ -z "${CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER:-}" ]]; then
  CANDIDATE_ROOTS=(
    "${DEVECO_SDK_HOME:-}"
    "/Applications/DevEco-Studio.app/Contents/sdk"
    "$HOME/Applications/DevEco-Studio.app/Contents/sdk"
    "${OHOS_SDK_HOME:-}"
    "$HOME/Library/OpenHarmony/Sdk"
    "/opt/ohos-sdk"
  )
  for root in "${CANDIDATE_ROOTS[@]}"; do
    [[ -n "$root" ]] || continue
    candidate="$(find "$root" -maxdepth 6 -type f -name 'aarch64-unknown-linux-ohos-clang' 2>/dev/null | head -1)"
    if [[ -n "$candidate" ]]; then
      export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER="$candidate"
      echo "[build-harmony] OHOS linker: $candidate"
      break
    fi
  done
fi

if [[ -z "${CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER:-}" ]]; then
  cat >&2 <<'MSG'
ERROR: HarmonyOS native toolchain not found (aarch64-unknown-linux-ohos-clang).

Install DevEco Studio (which bundles the HarmonyOS SDK) or the DevEco command
line tools, then either:
  export DEVECO_SDK_HOME=/Applications/DevEco-Studio.app/Contents/sdk
or point directly at the linker:
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER=<sdk>/native/llvm/bin/aarch64-unknown-linux-ohos-clang
MSG
  exit 1
fi

CARGO_ARGS=()
if [[ "$BUILD_MODE" == "release" ]]; then
    CARGO_ARGS+=(--release)
fi

echo "[build-harmony] Building phantom-harmony for ${TARGET} (${BUILD_MODE})"
cargo build --target "${TARGET}" "${CARGO_ARGS[@]}"

# ArkTS imports the NAPI module as `libphantom_harmony.so` from the
# standard native-libs location: entry/libs/arm64-v8a/. (rawfile is NOT
# on the NAPI search path — copying there silently ships a stale .so.)
OUTPUT_DIR="${HARMONY_DIR}/entry/libs/arm64-v8a"
mkdir -p "$OUTPUT_DIR"

if [[ "$BUILD_MODE" == "release" ]]; then
    cp "${PROJECT_ROOT}/target/${TARGET}/release/libphantom_harmony.so" \
       "${OUTPUT_DIR}/libphantom_harmony.so"
else
    cp "${PROJECT_ROOT}/target/${TARGET}/debug/libphantom_harmony.so" \
       "${OUTPUT_DIR}/libphantom_harmony.so"
fi

echo "[build-harmony] Copied libphantom_harmony.so to ${OUTPUT_DIR}"
echo "[build-harmony] Next: open ${HARMONY_DIR} in DevEco Studio and run."
