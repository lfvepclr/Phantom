#!/usr/bin/env bash
# scripts/build-mac.sh — Build Phantom.app for macOS from source.
#
# 参考 qoder/mytime 的 swift build + DMGBuilderExec 模式:
#   1. cargo build -p phantom-client --lib       (Rust cdylib)
#   2. cp dylib 到 client/mac/PhantomLibs/      (SPM linkerSettings 链接它)
#   3. swift build -c release                    (SPM 编译 PhantomMac + PhantomMacBuilder)
#   4. swift run PhantomMacBuilder               (把产物打成 .app 并 ad-hoc 签名)
#
# Usage:
#   scripts/build-mac.sh             # 默认 release
#   scripts/build-mac.sh --debug     # debug profile

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
MAC_DIR="$ROOT/client/mac"
LIB_DIR="$MAC_DIR/PhantomLibs"
DYLIB_NAME="libphantom_client.dylib"

if [[ "$PROFILE_FLAG" == "--release" ]]; then
  CARGO_TARGET_SUBDIR="release"
else
  CARGO_TARGET_SUBDIR="debug"
fi

echo "════════════════════════════════════════════════"
echo "  Phantom macOS Build"
echo "════════════════════════════════════════════════"

# Step 1: cargo build Rust cdylib
echo "[1/4] cargo build -p phantom-client --lib ..."
( cd "$ROOT" && cargo build $PROFILE_FLAG -p phantom-client --lib )

# Step 2: 复制 dylib 到 client/mac/PhantomLibs/，让 SPM linkerSettings 能找到
echo "[2/4] Copying dylib to client/mac/PhantomLibs/ ..."
mkdir -p "$LIB_DIR"
cp "$ROOT/target/$CARGO_TARGET_SUBDIR/$DYLIB_NAME" "$LIB_DIR/$DYLIB_NAME"
ls -l "$LIB_DIR/$DYLIB_NAME"

# Step 3: swift build (SPM 编译 PhantomMac + PhantomMacBuilder)
echo "[3/4] swift build -c release ..."
( cd "$MAC_DIR" && swift build -c release )

# Step 4: 跑 bundler 生成 Phantom.app
echo "[4/4] swift run PhantomMacBuilder ..."
( cd "$MAC_DIR" && swift run -c release PhantomMacBuilder )

echo ""
echo "════════════════════════════════════════════════"
echo "  ✅ Phantom.app + DMG are ready"
echo "════════════════════════════════════════════════"
echo "  App  : $MAC_DIR/Phantom.app"
echo "  DMG  : $MAC_DIR/dist/Phantom.dmg"
echo ""
echo "  Quick launch (TUN requires root):"
echo "    sudo open $MAC_DIR/Phantom.app"
echo ""
echo "  Or install via DMG (avoids Gatekeeper prompts):"
echo "    open $MAC_DIR/dist/Phantom.dmg"
echo "    # then drag Phantom.app into /Applications"
echo "════════════════════════════════════════════════"