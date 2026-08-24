#!/usr/bin/env bash
# Phase D full-matrix benchmark: cipher x protocol x weak-network.
# Reads credentials from .e2e-boot/uri.txt; expects the four containers
# (phantom-server/-quic/-2 + phantom-web) to be running.
#
# Usage: bash tests/e2e/bench-matrix.sh <tag>
set -uo pipefail
cd "$(dirname "$0")/../.."

TAG="${1:-matrix}"
URI_FILE=".e2e-boot/uri.txt"
KEY=$(grep -o 'phantom://[^@]*' "$URI_FILE" | sed 's|phantom://||')
PSK=$(grep -o 'psk=[^&]*' "$URI_FILE" | sed 's|psk=||')
LP=.e2e-ctx/lossy_proxy

run_client() { # port proto cipher
  local port=$1 proto=$2 cipher=$3
  pkill -f "phantom client" 2>/dev/null; sleep 0.5
  RUST_LOG=warn ./target/release/phantom client \
    -s "phantom://${KEY}@127.0.0.1:${port}?psk=${PSK}&cipher=${cipher}&proto=${proto}#${TAG}" \
    >/dev/null 2>&1 &
  sleep 3
}
stop_client() { pkill -f "phantom client" 2>/dev/null; sleep 0.3; }

dl() { # port proto cipher file label
  run_client "$1" "$2" "$3"
  curl -s -m 120 --socks5-hostname 127.0.0.1:1080 -o /dev/null \
    -w "BENCH[${TAG}] $5 %{speed_download} B/s %{http_code}\n" \
    "http://phantom-web:8080/$4"
  stop_client
}

echo "===== [D1] cipher 矩阵 × TCP 链路（10MB） ====="
for c in aes-256-gcm aes-128-gcm chacha20-poly1305 ascon-128; do
  for i in 1 2; do
    dl 443 tcp "$c" 10mb.bin "CIPHER-TCP-${c}-r$i"
  done
done

echo "===== [D2] cipher 矩阵 × QUIC 链路（10MB, 经 gvproxy 仅记录） ====="
for c in aes-256-gcm chacha20-poly1305; do
  dl 8443 quic "$c" 10mb.bin "CIPHER-QUIC-${c}"
done

echo "===== [D3] 弱网矩阵（lossy_proxy：TCP 仅延迟 / UDP 真丢包） ====="
pkill -f lossy_proxy 2>/dev/null
# 跨境优链路 100ms+1%: 9100=tcp 9101=udp
$LP 9100 127.0.0.1:443 100 1 tcp >/dev/null 2>&1 &
$LP 9101 127.0.0.1:8443 100 1 udp >/dev/null 2>&1 &
# 恶劣移动网 300ms+5%: 9000=tcp 9001=udp
$LP 9000 127.0.0.1:443 300 5 tcp >/dev/null 2>&1 &
$LP 9001 127.0.0.1:8443 300 5 udp >/dev/null 2>&1 &
sleep 1

for scene in "100ms1:9100:9101" "300ms5:9000:9001"; do
  name=${scene%%:*}; rest=${scene#*:}; tport=${rest%%:*}; qport=${rest##*:}
  for c in aes-256-gcm chacha20-poly1305; do
    run_client "$tport" tcp "$c"
    curl -s -m 300 --socks5-hostname 127.0.0.1:1080 -o /dev/null \
      -w "BENCH[${TAG}] WEAK-${name}-TCP-${c} %{speed_download} B/s %{http_code}\n" \
      http://phantom-web:8080/1mb.bin
    stop_client
    run_client "$qport" quic "$c"
    curl -s -m 300 --socks5-hostname 127.0.0.1:1080 -o /dev/null \
      -w "BENCH[${TAG}] WEAK-${name}-QUIC-${c} %{speed_download} B/s %{http_code}\n" \
      http://phantom-web:8080/1mb.bin
    stop_client
  done
done
pkill -f lossy_proxy 2>/dev/null

echo "===== [D4] 重联/闪断 ====="
# 服务端重启恢复窗口
run_client 443 tcp aes-256-gcm
T0=$(date +%s)
podman restart phantom-server >/dev/null 2>&1
ok_at=""
for i in $(seq 1 40); do
  R=$(curl -s -m 6 --socks5-hostname 127.0.0.1:1080 -o /dev/null -w "%{http_code}" http://phantom-web:8080/100kb.bin 2>/dev/null)
  if [ "$R" = "200" ]; then ok_at=$(( $(date +%s) - T0 )); break; fi
  sleep 1
done
echo "BENCH[${TAG}] RECONNECT-RESTART ${ok_at:-timeout}s-to-recover"
stop_client

echo "BENCH[${TAG}] DONE"
