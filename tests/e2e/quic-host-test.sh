#!/bin/bash
# E2E: QUIC 数据面（宿主机 → 8443 → phantom-server-quic 容器）
set -u
cd /Users/<user>/workspace/qoder/phantom

URI_QUIC='phantom://cGhhbnRvbS1lMmUtdGVzdC1zZXJ2ZXIta2V5LTMyYiE==@127.0.0.1:8443?psk=cGhhbnRvbS1lMmUtcHJlLXNoYXJlZC1rZXktMzJiISE==&cipher=auto&proto=quic#default'

RUST_LOG=info ./target/release/phantom client -s "$URI_QUIC" > .e2e-boot/client-quic2.log 2>&1 &
CPID=$!
sleep 5

echo '--- client log ---'
head -12 .e2e-boot/client-quic2.log

if kill -0 "$CPID" 2>/dev/null; then
  echo '--- SOCKS5 via QUIC ---'
  body=$(curl -s -m 10 --socks5-hostname 127.0.0.1:1080 http://phantom-web:8080/)
  echo "$body"
  echo "$body" | grep -q 'PHANTOM-E2E-OK' && echo '[PASS] socks5 QUIC' || echo '[FAIL] socks5 QUIC'

  echo '--- HTTP proxy via QUIC ---'
  body=$(curl -s -m 10 -x http://127.0.0.1:1080 http://phantom-web:8080/)
  echo "$body"
  echo "$body" | grep -q 'PHANTOM-E2E-OK' && echo '[PASS] http-proxy QUIC' || echo '[FAIL] http-proxy QUIC'

  echo '--- metrics ---'
  curl -s -m 5 http://127.0.0.1:9150/metrics | grep -E '^phantom' | head -8
else
  echo '[FAIL] client exited:'
  cat .e2e-boot/client-quic2.log
fi

kill "$CPID" 2>/dev/null
wait "$CPID" 2>/dev/null
echo ''
echo '=== quic server logs ==='
podman logs phantom-server-quic 2>&1 | grep -E 'QUIC|Client connected|SYN|Relay' | tail -10
