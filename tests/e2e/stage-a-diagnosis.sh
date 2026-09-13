#!/bin/bash
# 阶段 A: QUIC 慢因分层诊断
#   L1: VM 内直连容器（绕过 gvproxy）QUIC + TCP 吞吐
#   对照: 宿主机经 gvproxy（gvproxy 转发层开销）
set -u
cd "$(dirname "$0")/../.."

KEY="${PHANTOM_E2E_KEY:-cGhhbnRvbS1lMmUtdGVzdC1zZXJ2ZXIta2V5LTMyYiE=}"
PSK="${PHANTOM_E2E_PSK:-cGhhbnRvbS1lMmUtcHJlLXNoYXJlZC1rZXktMzJiISE=}"

TCP_SRV=$(podman inspect -f '{{(index .NetworkSettings.Networks "phantom-net").IPAddress}}' phantom-server)
QUIC_SRV=$(podman inspect -f '{{(index .NetworkSettings.Networks "phantom-net").IPAddress}}' phantom-server-quic)
echo "tcp container: $TCP_SRV   quic container: $QUIC_SRV"

# musl 客户端上轮已传 /root/phantom；重新传输保证最新（base64 通道防 PTY 损坏）
base64 < target/aarch64-unknown-linux-musl/release/phantom | \
  podman machine ssh "base64 -d > /root/phantom && chmod +x /root/phantom && echo transfer-ok"

vm_round() {
  local label="$1" host="$2" proto="$3"
  echo "===== L1-VM-DIRECT $label ====="
  podman machine ssh "
    pkill -f 'phantom client' 2>/dev/null; sleep 0.5
    RUST_LOG=warn /root/phantom client -s 'phantom://${KEY}@${host}:443?psk=${PSK}&cipher=auto&proto=${proto}#vm' > /root/c.log 2>&1 &
    sleep 4
    if ! pgrep -f 'phantom client' > /dev/null; then
      echo '[FAIL] client died:'; cat /root/c.log; exit 1
    fi
    curl -s -m 120 --socks5-hostname 127.0.0.1:1080 -o /dev/null \
      -w '  VM-direct $label speed=%{speed_download} B/s size=%{size_download} time=%{time_total}s\n' \
      http://phantom-web:8080/10mb.bin
    pkill -f 'phantom client'; true
  "
}

echo '===== L2-HOST-BASELINE (经 gvproxy) ====='
host_round() {
  local label="$1" port="$2" proto="$3"
  echo "--- host $label ---"
  pkill -f 'phantom client' 2>/dev/null; sleep 0.5
  RUST_LOG=warn ./target/release/phantom client -s "phantom://${KEY}@127.0.0.1:${port}?psk=${PSK}&cipher=auto&proto=${proto}#host" > /dev/null 2>&1 &
  local pid=$!
  sleep 4
  if kill -0 "$pid" 2>/dev/null; then
    curl -s -m 120 --socks5-hostname 127.0.0.1:1080 -o /dev/null \
      -w "  host-via-gvproxy $label speed=%{speed_download} B/s size=%{size_download} time=%{time_total}s\n" \
      http://phantom-web:8080/10mb.bin
  else
    echo "[FAIL] host client died"
  fi
  kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
}

host_round TCP  443 tcp
host_round QUIC 8443 quic
vm_round TCP  "$TCP_SRV"  tcp
vm_round QUIC "$QUIC_SRV" quic

echo ''
echo '===== server logs (cipher confirmation) ====='
podman logs phantom-server 2>&1 | grep 'Client connected' | tail -2
podman logs phantom-server-quic 2>&1 | grep -E 'QUIC client' | tail -2
