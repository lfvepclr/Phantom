#!/bin/bash
# E2E: CLI 客户端数据面测试（QUIC 轮 + TCP 轮 + HTTP 代理 + 日志断言）
set -u
cd "$(dirname "$0")/../.."

# 凭据从环境变量读取，仓库里只留占位值（PHANTOM_E2E_KEY / PHANTOM_E2E_PSK）。
KEY="${PHANTOM_E2E_KEY:-cGhhbnRvbS1lMmUtdGVzdC1zZXJ2ZXIta2V5LTMyYiE=}"
E2E_PSK="${PHANTOM_E2E_PSK:-cGhhbnRvbS1lMmUtcHJlLXNoYXJlZC1rZXktMzJiISE=}"
URI_BASE="phantom://${KEY}@127.0.0.1:443?psk=${E2E_PSK}&cipher=auto"
URI_QUIC="${URI_BASE}&proto=quic#default"
URI_TCP="${URI_BASE}&proto=tcp#default"

run_round() {
  local label="$1" uri="$2" logfile="$3"
  echo "===================== $label ====================="
  RUST_LOG=info ./target/release/phantom client -s "$uri" > "$logfile" 2>&1 &
  local pid=$!
  sleep 3
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "[FAIL] client exited early:"
    cat "$logfile"
    return 1
  fi
  echo "--- client log (head) ---"
  head -12 "$logfile"

  echo "--- SOCKS5 GET index ---"
  local body
  body=$(curl -s -m 10 --socks5-hostname 127.0.0.1:1080 http://phantom-web:8080/)
  echo "$body"
  if echo "$body" | grep -q 'PHANTOM-E2E-OK'; then echo "[PASS] socks5 $label"; else echo "[FAIL] socks5 $label"; fi

  echo "--- HTTP proxy GET index ---"
  body=$(curl -s -m 10 -x http://127.0.0.1:1080 http://phantom-web:8080/)
  echo "$body"
  if echo "$body" | grep -q 'PHANTOM-E2E-OK'; then echo "[PASS] http-proxy $label"; else echo "[FAIL] http-proxy $label"; fi

  echo "--- metrics ---"
  curl -s -m 5 http://127.0.0.1:9150/metrics | grep -E '^phantom' | head -8

  kill "$pid" 2>/dev/null
  wait "$pid" 2>/dev/null
  echo ""
}

run_round "QUIC" "$URI_QUIC" .e2e-boot/client-quic.log
sleep 1
run_round "TCP"  "$URI_TCP"  .e2e-boot/client-tcp.log

echo "===================== server logs ====================="
podman logs phantom-server 2>&1 | grep -E 'Client connected|QUIC client|SYN|Relay done|Connect failed' | tail -20
