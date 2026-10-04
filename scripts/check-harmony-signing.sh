#!/usr/bin/env bash
# 鸿蒙签名材料有效期预检。
#
# 背景：DevEco「自动签名」生成的调试 Profile 只有约 14 天有效期。材料过期后，
# HarmonyOS 6.x/7.x 真机会把应用判为「应用不可用」（aa start 报 10106105），
# 而且 AppGallery 弹窗只会引导卸载，不会提示"证书过期"。
#
# 退出码：
#   0  材料有效；或本机未配置签名材料（干净的 CI checkout，跳过检查）
#   1  Profile 已过期或无法解析
#
# 用法：
#   scripts/check-harmony-signing.sh
#   scripts/check-harmony-signing.sh --json
#   scripts/check-harmony-signing.sh --profile /path/app.p7b --cert /path/app.cer
#   HARMONY_SIGNING_PROFILE=/path/app.p7b HARMONY_SIGNING_CERT=/path/app.cer \
#     scripts/check-harmony-signing.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUILD_PROFILE="${HARMONY_BUILD_PROFILE:-$ROOT/client/harmony/build-profile.json5}"
PROFILE="${HARMONY_SIGNING_PROFILE:-}"
CERT="${HARMONY_SIGNING_CERT:-}"
WARN_DAYS="${HARMONY_SIGNING_WARN_DAYS:-7}"
AS_JSON=0

usage() {
  cat <<'EOF'
用法: scripts/check-harmony-signing.sh [--json] [--warn-days N]
       [--profile app.p7b] [--cert app.cer]

从 client/harmony/build-profile.json5 自动读取本机 signingConfigs 里的
certpath/profile；也可以用参数或环境变量覆盖：
  --profile / HARMONY_SIGNING_PROFILE   p7b Profile 路径
  --cert    / HARMONY_SIGNING_CERT      应用证书路径（可选，仅用于报告文件名）
  HARMONY_SIGNING_WARN_DAYS             提前预警天数（默认 7）

退出码 0=有效/未配置，1=已过期或解析失败。
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --json) AS_JSON=1 ;;
    --profile)
      [[ $# -ge 2 ]] || { echo "missing value for --profile" >&2; exit 2; }
      PROFILE="$2"
      shift
      ;;
    --cert)
      [[ $# -ge 2 ]] || { echo "missing value for --cert" >&2; exit 2; }
      CERT="$2"
      shift
      ;;
    --warn-days)
      [[ $# -ge 2 ]] || { echo "missing value for --warn-days" >&2; exit 2; }
      WARN_DAYS="$2"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

# 1) 未显式指定时，从 build-profile.json5 的 signingConfigs 里取第一个 profile/cert。
if [[ -f "$BUILD_PROFILE" ]]; then
  SIGNING_PATHS="$(python3 - "$BUILD_PROFILE" <<'PY'
import re, sys
try:
    text = open(sys.argv[1], encoding="utf-8").read()
except OSError:
    text = ""
profile = re.search(r'"profile"\s*:\s*"([^"]+)"', text)
cert = re.search(r'"certpath"\s*:\s*"([^"]+)"', text)
print(profile.group(1) if profile else "")
print(cert.group(1) if cert else "")
PY
)"
  [[ -z "$PROFILE" ]] && PROFILE="$(printf '%s\n' "$SIGNING_PATHS" | sed -n '1p')"
  [[ -z "$CERT" ]] && CERT="$(printf '%s\n' "$SIGNING_PATHS" | sed -n '2p')"
fi

if [[ -z "$PROFILE" ]]; then
  if [[ "$AS_JSON" == "1" ]]; then
    printf '{"configured":false,"status":"skipped"}\n'
  else
    echo "[check-harmony-signing] 本机未配置 HarmonyOS signingConfigs（CI/干净 checkout），跳过。"
    echo "[check-harmony-signing] 真机调试前请在 DevEco 里开启自动签名，或设置 HARMONY_SIGNING_PROFILE。"
  fi
  exit 0
fi

# 2) 当前连接的设备 UDID（可选）：调试 Profile 必须绑定该 UDID。
DEVICE_UDID=""
if command -v hdc >/dev/null 2>&1; then
  HDC_TARGET="$(hdc list targets 2>/dev/null | tr -d '\r' | head -1 || true)"
  if [[ -n "$HDC_TARGET" ]]; then
    DEVICE_UDID="$(hdc -t "$HDC_TARGET" shell bm get -u 2>/dev/null | tr -d '\r' | tail -1 || true)"
  fi
fi

python3 - "$PROFILE" "$CERT" "$WARN_DAYS" "$AS_JSON" "$DEVICE_UDID" <<'PY'
import datetime as dt
import json
import os
import sys

profile_path, cert_path, warn_days_s, as_json_s, device_udid = sys.argv[1:6]
warn_days = float(warn_days_s)
as_json = as_json_s == "1"


def fail(message, code=1):
    if as_json:
        print(json.dumps({"configured": True, "status": "error", "error": message},
                         ensure_ascii=False))
    else:
        print(f"[check-harmony-signing] ERROR: {message}", file=sys.stderr)
    sys.exit(code)


if not os.path.isfile(profile_path):
    fail(f"Profile 不存在: {profile_path}")

# p7b 是 PKCS#7 SignedData，内嵌的 profile JSON 在二进制里以明文出现。
# 从 {"version-name" 开始做一次带字符串感知的括号配对，即可取出 payload。
raw = open(profile_path, "rb").read().decode("latin-1", errors="ignore")
start = raw.find('{"version-name"')
if start < 0:
    fail(f"无法在 {profile_path} 中找到 profile payload（不是预期的 p7b？）")

depth = 0
in_string = False
escaped = False
end = -1
for i in range(start, len(raw)):
    ch = raw[i]
    if in_string:
        if escaped:
            escaped = False
        elif ch == "\\":
            escaped = True
        elif ch == '"':
            in_string = False
        continue
    if ch == '"':
        in_string = True
    elif ch == "{":
        depth += 1
    elif ch == "}":
        depth -= 1
        if depth == 0:
            end = i + 1
            break

if end < 0:
    fail("profile payload JSON 括号不闭合")

try:
    payload = json.loads(raw[start:end])
except json.JSONDecodeError as exc:
    fail(f"profile payload JSON 解析失败: {exc}")

validity = payload.get("validity", {})
not_before = validity.get("not-before")
not_after = validity.get("not-after")
if not isinstance(not_before, int) or not isinstance(not_after, int):
    fail("profile 缺少 validity.not-before/not-after")

now = dt.datetime.now().timestamp()
remaining_s = not_after - now
remaining_days = remaining_s / 86400.0
expired = now > not_after
expiring_soon = (not expired) and (remaining_s < warn_days * 86400)

profile_type = payload.get("type", "unknown")
bundle_info = payload.get("bundle-info", {}) or {}
bundle_name = bundle_info.get("bundle-name", "")
debug_info = payload.get("debug-info", {}) or {}
device_ids = debug_info.get("device-ids", []) or []
device_match = None
if device_udid and device_ids:
    device_match = device_udid in device_ids

# DevEco 自动签名材料大约 14 天有效。
auto_sign = profile_type == "debug" and (not_after - not_before) <= 20 * 86400


def fmt(ts):
    return dt.datetime.fromtimestamp(ts).astimezone().strftime("%Y-%m-%d %H:%M:%S %Z")


advice = []
if expired:
    advice.append("材料已过期：在 DevEco 里重新执行自动签名（约 +14 天），或改用 AGC 的调试证书(1年)/发布证书(3年)。")
    advice.append("重签后先覆盖安装；若仍被管控，再卸载后重装以清掉 AppGallery 的旧管控状态。")
elif expiring_soon:
    advice.append(f"将在 {remaining_days:.1f} 天内过期，请在断网/换机前重新签名。")
if auto_sign:
    advice.append("这是 DevEco 自动签发的调试材料（约 14 天），不是长期签名方案。")

status = "expired" if expired else ("expiring" if expiring_soon else "ok")
result = {
    "configured": True,
    "status": status,
    "profile": profile_path,
    "cert": cert_path or None,
    "type": profile_type,
    "auto_sign": auto_sign,
    "bundle_name": bundle_name,
    "not_before": not_before,
    "not_after": not_after,
    "not_before_local": fmt(not_before),
    "not_after_local": fmt(not_after),
    "remaining_days": round(remaining_days, 2),
    "device_udid": device_udid or None,
    "profile_device_ids": len(device_ids),
    "device_udid_match": device_match,
    "advice": advice,
}

if as_json:
    print(json.dumps(result, ensure_ascii=False))
else:
    print(f"[check-harmony-signing] profile : {profile_path}")
    print(f"[check-harmony-signing] type    : {profile_type}"
          + ("（DevEco 自动签名，约 14 天）" if auto_sign else ""))
    if bundle_name:
        print(f"[check-harmony-signing] bundle  : {bundle_name}")
    print(f"[check-harmony-signing] validity: {fmt(not_before)} → {fmt(not_after)}")
    if expired:
        print(f"[check-harmony-signing] status  : EXPIRED（已过期 {-remaining_days:.1f} 天）")
    elif expiring_soon:
        print(f"[check-harmony-signing] status  : WARN（剩余 {remaining_days:.1f} 天）")
    else:
        print(f"[check-harmony-signing] status  : OK（剩余 {remaining_days:.1f} 天）")
    if device_udid and device_ids:
        verdict = "匹配" if device_match else "不匹配"
        print(f"[check-harmony-signing] device  : 当前 UDID 与 Profile {verdict}")
    for line in advice:
        print(f"[check-harmony-signing] 提示    : {line}")

sys.exit(1 if expired else 0)
PY
