#!/bin/bash
# 基准套件：微基准 + 链路级吞吐 + 弱网。每个优化 Phase 前后各跑一次。
# 用法: bench-suite.sh <phase-tag>
# 输出行前缀 BENCH[<phase-tag>] 便于 grep 汇总对比
set -u
cd "$(dirname "$0")/../.."

TAG="${1:-x}"
KEY=$(sed -E 's|phantom://([^@]+)@.*|\1|' .e2e-boot/uri.txt)
PSK=$(sed -E 's|.*psk=([^&]+)&.*|\1|' .e2e-boot/uri.txt)
LP=.e2e-ctx/lossy_proxy

# 弱网代理按需编译
[ -x "$LP" ] || rustc --edition 2021 -O -o "$LP" tests/e2e/lossy_proxy.rs 2>/dev/null

run_client() {
  local port="$1" proto="$2"
  pkill -f 'phantom client' 2>/dev/null; sleep 0.5
  RUST_LOG=warn ./target/release/phantom client -s \
    "phantom://${KEY}@127.0.0.1:${port}?psk=${PSK}&cipher=auto&proto=${proto}#${TAG}" > /dev/null 2>&1 &
  CPID=$!
  sleep 4
}

stop_client() { pkill -f 'phantom client' 2>/dev/null; }

echo "===== [1] 微基准: macOS 本机（64KB 块） ====="
./target/release/examples/cipher_bench 2>/dev/null | awk -v t="$TAG" '$2==65536 {print "BENCH["t"] MICRO-MAC "$1" enc="$3" dec="$4" MiB/s"}'

echo "===== [2] 微基准: 1vCPU 容器 (musl)（64KB 块） ====="
podman run --rm --cpus 1 --entrypoint /cb \
  -v "$PWD/target/aarch64-unknown-linux-musl/release/examples/cipher_bench:/cb:Z" \
  localhost/phantom-server 2>/dev/null | awk -v t="$TAG" '$2==65536 {print "BENCH["t"] MICRO-CTR "$1" enc="$3" dec="$4" MiB/s"}'

echo "===== [3] 链路级吞吐: TCP 10MB x3 ====="
run_client 443 tcp
for i in 1 2 3; do
  curl -s -m 120 --socks5-hostname 127.0.0.1:1080 -o /dev/null \
    -w "BENCH[${TAG}] LINK-TCP run${i} %{speed_download} B/s %{http_code}\n" \
    http://phantom-web:8080/10mb.bin
done
stop_client

echo "===== [4] 链路级吞吐: QUIC 10MB x3（经 gvproxy，仅记录） ====="
run_client 8443 quic
for i in 1 2 3; do
  curl -s -m 120 --socks5-hostname 127.0.0.1:1080 -o /dev/null \
    -w "BENCH[${TAG}] LINK-QUIC run${i} %{speed_download} B/s %{http_code}\n" \
    http://phantom-web:8080/10mb.bin
done
stop_client

echo "===== [5] 弱网: 300ms+5%（QUIC 真丢包 / TCP 仅延迟） ====="
pkill -f lossy_proxy 2>/dev/null
$LP 9000 127.0.0.1:443 300 0 tcp > /dev/null 2>&1 &
$LP 9001 127.0.0.1:8443 300 5 udp > /dev/null 2>&1 &
sleep 1
run_client 9000 tcp
curl -s -m 300 --socks5-hostname 127.0.0.1:1080 -o /dev/null \
  -w "BENCH[${TAG}] WEAK-300ms5-TCP %{speed_download} B/s %{http_code}\n" \
  http://phantom-web:8080/1mb.bin
stop_client
run_client 9001 quic
curl -s -m 300 --socks5-hostname 127.0.0.1:1080 -o /dev/null \
  -w "BENCH[${TAG}] WEAK-300ms5-QUIC %{speed_download} B/s %{http_code}\n" \
  http://phantom-web:8080/1mb.bin
stop_client
pkill -f lossy_proxy 2>/dev/null

echo "===== [6] 弱网: 限速 10mbit tbf ====="
# veth 定位: podman inspect MAC -> VM 内 bridge fdb（网络名含连字符须用 index 模板）
MAC=$(podman inspect phantom-server --format '{{(index .NetworkSettings.Networks "phantom-net").MacAddress}}')
VETH=$(podman machine ssh "bridge fdb show br podman1" 2>/dev/null | grep -i "$MAC" | awk '{print $3}' | head -1)
echo "veth=${VETH:-not-found}"
if [ -n "$VETH" ]; then
  podman machine ssh "tc qdisc replace dev $VETH root tbf rate 10mbit burst 64kbit latency 400ms" 2>/dev/null
  sleep 1
  run_client 443 tcp
  curl -s -m 300 --socks5-hostname 127.0.0.1:1080 -o /dev/null \
    -w "BENCH[${TAG}] WEAK-10mbit-TCP %{speed_download} B/s %{http_code}\n" \
    http://phantom-web:8080/1mb.bin
  stop_client
  run_client 8443 quic
  curl -s -m 300 --socks5-hostname 127.0.0.1:1080 -o /dev/null \
    -w "BENCH[${TAG}] WEAK-10mbit-QUIC %{speed_download} B/s %{http_code}\n" \
    http://phantom-web:8080/1mb.bin
  stop_client
  podman machine ssh "tc qdisc del dev $VETH root" 2>/dev/null
fi
echo "BENCH[${TAG}] DONE"
