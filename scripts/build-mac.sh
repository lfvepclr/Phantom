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
#   scripts/build-mac.sh --install   # 额外装到 /Applications 并刷新图标缓存

set -euo pipefail

PROFILE_FLAG="--release"
INSTALL=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --debug) PROFILE_FLAG=""; shift ;;
    --install) INSTALL=1; shift ;;
    -h|--help) sed -n '2,13p' "$0"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
ROOT="$SCRIPT_DIR/.."
MAC_DIR="$ROOT/client/mac"
LIB_DIR="$MAC_DIR/.build/lib"
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

# Step 2: Copy dylib to client/mac/.build/lib/, where SPM linkerSettings looks for it
 echo "[2/4] Copying dylib to client/mac/.build/lib/ ..."
mkdir -p "$LIB_DIR"
cp "$ROOT/target/$CARGO_TARGET_SUBDIR/$DYLIB_NAME" "$LIB_DIR/$DYLIB_NAME"
ls -l "$LIB_DIR/$DYLIB_NAME"

# Step 3: swift build (SPM 编译 PhantomMac + PhantomMacBuilder)
echo "[3/4] swift build -c release ..."
( cd "$MAC_DIR" && xcrun swift build -c release )

# Step 4: 跑 bundler 生成 Phantom.app
echo "[4/4] swift run PhantomMacBuilder ..."
( cd "$MAC_DIR" && xcrun swift run -c release PhantomMacBuilder )

# The Swift side now owns real logic (URI parsing, log filtering, whitelist
# validation, probes, menu bar glyph). Run its tests so a broken icon or a
# collapsed log pane cannot ship unnoticed.
echo "[5/5] swift test ..."
( cd "$MAC_DIR" && xcrun swift test )

if [[ "$INSTALL" == "1" ]]; then
    echo "[install] Copying to /Applications and refreshing the icon cache ..."
    rm -rf /Applications/Phantom.app
    cp -R "$MAC_DIR/.build/Phantom.app" /Applications/Phantom.app
    # Finder caches icons per bundle id, so a replaced bundle with a new
    # AppIcon.icns keeps showing the old artwork until LaunchServices is poked.
    touch /Applications/Phantom.app
    /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
        -f /Applications/Phantom.app || true
    echo "  Installed /Applications/Phantom.app (run 'killall Dock' if the old icon lingers)"
fi

echo ""
echo "════════════════════════════════════════════════"
echo "  ✅ Phantom.app + DMG are ready"
echo "════════════════════════════════════════════════"
echo "  App  : $MAC_DIR/.build/Phantom.app"
echo "  DMG  : $MAC_DIR/.build/dist/Phantom.dmg"
echo ""
echo "  Quick launch (no sudo needed):"
echo "    open $MAC_DIR/.build/Phantom.app"
echo "  Optional TUN/transparent mode (needs root; 'sudo open X.app' does NOT"
echo "  elevate — LaunchServices starts the bundle as the logged-in user):"
echo "    sudo $MAC_DIR/.build/Phantom.app/Contents/MacOS/Phantom"
echo ""
echo "  Or install via DMG (avoids Gatekeeper prompts):"
echo "    open $MAC_DIR/.build/dist/Phantom.dmg"
echo "    # then drag Phantom.app into /Applications"
echo "════════════════════════════════════════════════"
