#!/usr/bin/env bash
# 用 AGC 申请的个人签名材料对已构建的 unsigned HAP 签名（可选直接安装）。
#
# 默认材料（调试证书 + 调试 Profile，实名后 1 年，绑定设备 UDID）：
#   client/harmony/signing/phantom-debug.{p12,cer,p7b}
# 可选材料（发布证书 + 发布 Profile，3 年，但仅授权应用商店分发，
# hdc 本地安装可能被系统拒绝）：
#   client/harmony/signing/phantom-release.{p12,cer,p7b}
#
# 材料获取步骤见 client/harmony/docs/SIGNING_AND_STARTUP_TROUBLESHOOTING.md。
#
# 用法:
#   scripts/sign-harmony-personal.sh                 # 调试材料签名
#   scripts/sign-harmony-personal.sh --install       # 签名并 hdc install -r
#   scripts/sign-harmony-personal.sh --release       # 发布材料签名（本地安装实验）
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HARMONY_DIR="$ROOT/client/harmony"
SIGN_DIR="$HARMONY_DIR/signing"
SDK="${DEVECO_SDK_HOME:-/Applications/DevEco-Studio.app/Contents/sdk}"
SIGN_TOOL="$SDK/default/openharmony/toolchains/lib/hap-sign-tool.jar"
DEFAULT_UNSIGNED="$HARMONY_DIR/entry/build/default/outputs/default/entry-default-unsigned.hap"

MODE="debug"
INSTALL=0
UNSIGNED="$DEFAULT_UNSIGNED"
SIGNED=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --release) MODE="release" ;;
    --install) INSTALL=1 ;;
    --unsigned)
      [[ $# -ge 2 ]] || { echo "missing value for --unsigned" >&2; exit 2; }
      UNSIGNED="$2"
      shift
      ;;
    --signed)
      [[ $# -ge 2 ]] || { echo "missing value for --signed" >&2; exit 2; }
      SIGNED="$2"
      shift
      ;;
    -h|--help)
      sed -n '2,20p' "$0"
      exit 0
      ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

if [[ "$MODE" == "release" ]]; then
  P12="$SIGN_DIR/phantom-release.p12"
  CER="$SIGN_DIR/phantom-release.cer"
  P7B="$SIGN_DIR/phantom-release.p7b"
  ALIAS="phantom_release"
  PWD_VAR="PHANTOM_RELEASE_KEY_PWD"
else
  P12="$SIGN_DIR/phantom-debug.p12"
  CER="$SIGN_DIR/phantom-debug.cer"
  P7B="$SIGN_DIR/phantom-debug.p7b"
  ALIAS="phantom_debug"
  PWD_VAR="PHANTOM_DEBUG_KEY_PWD"
fi
[[ -n "$SIGNED" ]] || SIGNED="$HARMONY_DIR/entry/build/default/outputs/default/entry-default-personal-${MODE}-signed.hap"

# 密码从 gitignored 的本地文件读取，脚本本身不保存密钥口令。
if [[ -f "$SIGN_DIR/passwords.env" ]]; then
  # shellcheck disable=SC1090
  source "$SIGN_DIR/passwords.env"
fi

for f in "$SIGN_TOOL" "$UNSIGNED" "$P12" "$CER" "$P7B"; do
  if [[ ! -f "$f" ]]; then
    cat >&2 <<MSG
[sign-harmony-personal] 缺少文件: $f
[sign-harmony-personal]
[sign-harmony-personal] 先按文档把 AGC 下载的证书/Profile 放到 client/harmony/signing/：
[sign-harmony-personal]   $SIGN_DIR/phantom-${MODE}.cer
[sign-harmony-personal]   $SIGN_DIR/phantom-${MODE}.p7b
[sign-harmony-personal] 说明:
[sign-harmony-personal]   client/harmony/docs/SIGNING_AND_STARTUP_TROUBLESHOOTING.md
MSG
    exit 1
  fi
done

KEY_PWD="${!PWD_VAR:-}"
if [[ -z "$KEY_PWD" ]]; then
  echo "[sign-harmony-personal] 缺少 $PWD_VAR（放在 $SIGN_DIR/passwords.env）" >&2
  exit 1
fi

java -jar "$SIGN_TOOL" sign-app -mode localSign \
  -keyAlias "$ALIAS" -keyPwd "$KEY_PWD" \
  -appCertFile "$CER" \
  -profileFile "$P7B" -profileSigned 1 \
  -inFile "$UNSIGNED" -signAlg SHA256withECDSA \
  -keystoreFile "$P12" -keystorePwd "$KEY_PWD" \
  -outFile "$SIGNED"

java -jar "$SIGN_TOOL" verify-app -inFile "$SIGNED" \
  -outCertChain /tmp/phantom-personal.cer -outProfile /tmp/phantom-personal.p7b >/dev/null
"$ROOT/scripts/check-harmony-signing.sh" --profile "$P7B" --cert "$CER"
echo "[sign-harmony-personal] signed: $SIGNED"

if [[ "$INSTALL" == "1" ]]; then
  hdc install -r "$SIGNED"
  hdc shell aa start -a EntryAbility -b co.phantom.harmony
fi
