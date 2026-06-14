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

CARGO_ARGS=()
if [[ "$BUILD_MODE" == "release" ]]; then
    CARGO_ARGS+=(--release)
fi

echo "[build-harmony] Building phantom-harmony for ${TARGET} (${BUILD_MODE})"
cargo build --target "${TARGET}" "${CARGO_ARGS[@]}"

OUTPUT_DIR="${HARMONY_DIR}/entry/src/main/resources/rawfile"
mkdir -p "$OUTPUT_DIR"

if [[ "$BUILD_MODE" == "release" ]]; then
    cp "${PROJECT_ROOT}/target/${TARGET}/release/libphantom_harmony.so" \
       "${OUTPUT_DIR}/libphantom.so"
else
    cp "${PROJECT_ROOT}/target/${TARGET}/debug/libphantom_harmony.so" \
       "${OUTPUT_DIR}/libphantom.so"
fi

echo "[build-harmony] Copied libphantom.so to ${OUTPUT_DIR}"
echo "[build-harmony] Next: open ${HARMONY_DIR} in DevEco Studio and run."
