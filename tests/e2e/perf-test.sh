#!/bin/bash
# E2E 阶段 6: 性能 —— TCP/QUIC 隧道吞吐、延迟、直连基准、三方字节对账
set -u
cd "$(dirname "$0")/../.."

# 凭据从环境变量读取，仓库里只留占位值；跑真机/容器对测时导出
# PHANTOM_E2E_KEY / PHANTOM_E2E_PSK（与服务端 server.key 一致）。
KEY="${PHANTOM_E2E_KEY:-cGhhbnRvbS1lMmUtdGVzdC1zZXJ2ZXIta2V5LTMyYiE=}"
E2E_PSK="${PHANTOM_E2E_PSK:-cGhhbnRvbS1lMmUtcHJlLXNoYXJlZC1rZXktMzJiISE=}"
URI_BASE="phantom://${KEY}@127.0.0.1"
PSK="psk=${E2E_PSK}&cipher=auto"

perf_round() {
  local label="$1" uri="$2" srv="$3"
  echo "===================== $label ====================="
  RUST_LOG=info ./target/release/phantom client -s "$uri" > ".e2e-boot/perf-$label.log" 2>&1 &
  local pid=$!
  sleep 4

  echo "--- 10MB 吞吐 ---"
  curl -s -m 60 --socks5-hostname 127.0.0.1:1080 -o /dev/null \
    -w "speed=%{speed_download} B/s  size=%{size_download} B  time=%{time_total}s\n" \
    http://phantom-web:8080/10mb.bin

  echo "--- 100KB 延迟 x5 ---"
  for i in 1 2 3 4 5; do
    curl -s -m 20 --socks5-hostname 127.0.0.1:1080 -o /dev/null \
      -w "  t=%{time_total}s\n" http://phantom-web:8080/100kb.bin
  done

  echo "--- metrics（对账用） ---"
  curl -s -m 5 http://127.0.0.1:9150/metrics | grep -E '^phantom' | head -8

  kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
  sleep 1
  echo "--- $srv 服务端转发日志（最后几条） ---"
  podman logs "$srv" 2>&1 | grep -E 'Relay done|SYN' | tail -8
  echo ''
}

perf_round TCP  "${URI_BASE}:443?${PSK}&proto=tcp#perf"  phantom-server
perf_round QUIC "${URI_BASE}:8443?${PSK}&proto=quic#perf" phantom-server-quic

echo "===================== 直连基准（VM → web 容器，不经隧道） ====================="
WEB_IP=$(podman inspect -f '{{(index .NetworkSettings.Networks "phantom-net").IPAddress}}' phantom-web)
podman machine ssh "curl -s -m 60 -o /dev/null -w 'direct speed=%{speed_download} B/s  size=%{size_download} B  time=%{time_total}s\n' http://${WEB_IP}:8080/10mb.bin"

echo ''
echo '=== perf test done ==='
