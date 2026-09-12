#!/bin/bash
# 阶段 C v2: 弱网矩阵
#   QUIC: lossy_proxy(UDP) 真丢包+延迟 | TCP: lossy_proxy(TCP) 仅延迟 | 限速: VM 内 tbf
set -u
cd "$(dirname "$0")/../.."

KEY='cGhhbnRvbS1lMmUtdGVzdC1zZXJ2ZXIta2V5LTMyYiE=='
PSK='cGhhbnRvbS1lMmUtcHJlLXNoYXJlZC1rZXktMzJiISE=='
LP=.e2e-ctx/lossy_proxy

run_client_curl() {
  local port="$1" proto="$2" label="$3"
  pkill -f 'phantom client' 2>/dev/null; sleep 0.5
  RUST_LOG=warn ./target/release/phantom client -s \
    "phantom://${KEY}@127.0.0.1:${port}?psk=${PSK}&cipher=auto&proto=${proto}#w" > /dev/null 2>&1 &
  local cpid=$!
  sleep 4
  if kill -0 $cpid 2>/dev/null; then
    curl -s -m 300 --socks5-hostname 127.0.0.1:1080 -o /dev/null \
      -w "WEAK [${label}] ${proto} speed=%{speed_download} B/s time=%{time_total}s\n" \
      http://phantom-web:8080/1mb.bin
  else
    echo "WEAK [${label}] ${proto} [FAIL] client died"
  fi
  kill $cpid 2>/dev/null; wait $cpid 2>/dev/null
}

echo '===== 场景1: 跨境优链路 delay 100ms loss 1% ====='
pkill -f lossy_proxy 2>/dev/null
$LP 9000 127.0.0.1:443 100 0 tcp > /dev/null 2>&1 &
$LP 9001 127.0.0.1:8443 100 1 udp > /dev/null 2>&1 &
sleep 1
run_client_curl 9000 tcp '100ms+1%(tcp-delay)'
run_client_curl 9001 quic '100ms+1%(udp-loss)'
pkill -f lossy_proxy 2>/dev/null

echo '===== 场景2: 恶劣移动网 delay 300ms loss 5% ====='
$LP 9000 127.0.0.1:443 300 0 tcp > /dev/null 2>&1 &
$LP 9001 127.0.0.1:8443 300 5 udp > /dev/null 2>&1 &
sleep 1
run_client_curl 9000 tcp '300ms+5%(tcp-delay)'
run_client_curl 9001 quic '300ms+5%(udp-loss)'
pkill -f lossy_proxy 2>/dev/null

echo '===== 场景3: 限速 10mbit（VM 内 tbf，协议无关） ====='
VETH=veth1
podman machine ssh "tc qdisc replace dev $VETH root tbf rate 10mbit burst 64kbit latency 400ms" 2>/dev/null
sleep 1
run_client_curl 443 tcp '10mbit'
run_client_curl 8443 quic '10mbit'
podman machine ssh "tc qdisc del dev $VETH root" 2>/dev/null

echo '===== 场景4: 恶劣网 cipher 对照（TCP delay 300ms） ====='
$LP 9000 127.0.0.1:443 300 0 tcp > /dev/null 2>&1 &
sleep 1
for c in aes256-gcm cha-cha20-poly1305 ascon128; do
  sed -i '' "s/^cipher = .*/cipher = \"${c}\"/" .e2e-data/server.toml
  podman restart phantom-server > /dev/null; sleep 3
  pkill -f 'phantom client' 2>/dev/null; sleep 0.5
  RUST_LOG=warn ./target/release/phantom client -s \
    "phantom://${KEY}@127.0.0.1:9000?psk=${PSK}&cipher=auto&proto=tcp#w" > /dev/null 2>&1 &
  cpid=$!
  sleep 4
  if kill -0 $cpid 2>/dev/null; then
    curl -s -m 300 --socks5-hostname 127.0.0.1:1080 -o /dev/null \
      -w "WEAK [300ms-delay] TCP-${c} speed=%{speed_download} B/s time=%{time_total}s\n" \
      http://phantom-web:8080/1mb.bin
  else
    echo "WEAK [300ms-delay] TCP-${c} [FAIL]"
  fi
  kill $cpid 2>/dev/null; wait $cpid 2>/dev/null
done
sed -i '' 's/^cipher = .*/cipher = "auto"/' .e2e-data/server.toml
podman restart phantom-server > /dev/null
pkill -f lossy_proxy 2>/dev/null
echo '=== stage C done ==='
