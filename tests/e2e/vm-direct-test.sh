#!/bin/bash
# 在 VM 内执行：直连容器测 QUIC/TCP 吞吐（绕过 gvproxy）
set -u
KEY='cGhhbnRvbS1lMmUtdGVzdC1zZXJ2ZXIta2V5LTMyYiE=='
PSK='cGhhbnRvbS1lMmUtcHJlLXNoYXJlZC1rZXktMzJiISE=='
TCP_SRV="$1"
QUIC_SRV="$2"

run() {
  local label="$1" host="$2" proto="$3"
  pkill -f 'phantom client' 2>/dev/null || true
  sleep 0.5
  RUST_LOG=warn /root/phantom client -s "phantom://${KEY}@${host}:443?psk=${PSK}&cipher=auto&proto=${proto}#vm" > /root/c.log 2>&1 &
  local pid=$!
  sleep 4
  if kill -0 "$pid" 2>/dev/null; then
    curl -s -m 120 --socks5-hostname 127.0.0.1:1080 -o /dev/null \
      -w "VM-direct ${label} speed=%{speed_download} B/s size=%{size_download} time=%{time_total}s\n" \
      http://phantom-web:8080/10mb.bin
  else
    echo "VM-direct ${label} [FAIL] client died:"
    cat /root/c.log
  fi
  pkill -f 'phantom client' 2>/dev/null || true
}

run TCP  "$TCP_SRV"  tcp
run QUIC "$QUIC_SRV" quic
# QUIC 多跑两轮观察波动
run QUIC2 "$QUIC_SRV" quic
